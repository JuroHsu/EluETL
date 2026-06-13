import { Injectable, signal } from "@angular/core";

import { SourcePreview } from "./tauri.service";

/**
 * 跨頁共用狀態（匯入 → 遷移作業）。
 * 單純 signal 容器；所有 IPC 呼叫仍走 TauriService。
 */
@Injectable({ providedIn: "root" })
export class EtlStateService {
  /** 來源型態：檔案（Excel / CSV）或資料庫查詢 */
  readonly sourceKind = signal<"file" | "database">("file");

  // ---- 檔案來源 ----
  readonly sourcePath = signal<string | null>(null);
  readonly sheets = signal<string[]>([]);
  readonly sheet = signal<string>("");
  readonly hasHeader = signal<boolean>(true);
  /** CSV 編碼覆寫（null = 自動偵測） */
  readonly encoding = signal<string | null>(null);

  // ---- 資料庫來源 ----
  readonly dbConnId = signal<string | null>(null);
  /** 選擇的資料表（"" = 自訂 SQL） */
  readonly dbTable = signal<string>("");
  /** 自訂 SQL 的編輯內容 */
  readonly dbCustomSql = signal<string>("");
  /** 實際採用的查詢（後端解析回傳，存入任務設定 / 腳本產生） */
  readonly dbQuery = signal<string>("");

  readonly preview = signal<SourcePreview | null>(null);

  /** 遷移作業的腳本內容（跨頁保留；視覺編輯模型由腳本文字往返） */
  readonly scriptText = signal<string>("");

  resetSource(): void {
    this.sourceKind.set("file");
    this.sourcePath.set(null);
    this.sheets.set([]);
    this.sheet.set("");
    this.encoding.set(null);
    this.dbConnId.set(null);
    this.dbTable.set("");
    this.dbCustomSql.set("");
    this.dbQuery.set("");
    this.preview.set(null);
  }
}
