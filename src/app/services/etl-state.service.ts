import { Injectable, signal } from "@angular/core";

import {
  EtlJobConfig,
  EtlSummary,
  SourcePreview,
} from "./tauri.service";

/**
 * 匯入精靈的跨頁狀態（匯入 → 對應 → 執行）。
 * 單純 signal 容器；所有 IPC 呼叫仍走 TauriService。
 */
@Injectable({ providedIn: "root" })
export class EtlStateService {
  readonly sourcePath = signal<string | null>(null);
  readonly sheets = signal<string[]>([]);
  readonly sheet = signal<string>("");
  readonly hasHeader = signal<boolean>(true);
  /** CSV 編碼覆寫（null = 自動偵測） */
  readonly encoding = signal<string | null>(null);
  readonly preview = signal<SourcePreview | null>(null);

  readonly job = signal<EtlJobConfig | null>(null);
  readonly summary = signal<EtlSummary | null>(null);

  /** 腳本模式的編輯內容（跨頁保留） */
  readonly scriptText = signal<string>("");

  resetSource(): void {
    this.sourcePath.set(null);
    this.sheets.set([]);
    this.sheet.set("");
    this.encoding.set(null);
    this.preview.set(null);
    this.job.set(null);
    this.summary.set(null);
  }
}
