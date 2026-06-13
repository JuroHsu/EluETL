import { Injectable, signal } from "@angular/core";

import { EtlProgress, EtlSummary, ScriptIssue } from "./tauri.service";

/** 底部輸出面板的分頁。 */
export type OutputTab = "log" | "diagnostics" | "result";

/**
 * 底部輸出面板的共用狀態。
 * 「遷移作業」頁產生診斷 / 進度 / 結果並寫入這裡，由 app 外殼的輸出面板以分頁呈現。
 * （log 由 LogService 提供；本服務僅管面板開關、目前分頁與診斷/結果資料。）
 */
@Injectable({ providedIn: "root" })
export class OutputService {
  /** 面板展開 / 收合 */
  readonly panelOpen = signal(true);
  readonly activeTab = signal<OutputTab>("log");

  // ---- 診斷分頁 ----
  readonly issues = signal<ScriptIssue[]>([]);
  /** 驗證成功訊息（null = 未驗證 / 有錯誤） */
  readonly checkMessage = signal<string | null>(null);

  // ---- 結果分頁 ----
  readonly running = signal(false);
  readonly progress = signal<EtlProgress | null>(null);
  readonly summary = signal<EtlSummary | null>(null);
  /** 執行期例外（連線 / 載入失敗等） */
  readonly error = signal<string | null>(null);

  /** 切到指定分頁並展開面板。 */
  show(tab: OutputTab): void {
    this.activeTab.set(tab);
    this.panelOpen.set(true);
  }

  clearDiagnostics(): void {
    this.issues.set([]);
    this.checkMessage.set(null);
  }

  clearResult(): void {
    this.progress.set(null);
    this.summary.set(null);
    this.error.set(null);
  }

  statusLabel(s: EtlSummary["status"]): string {
    return (
      { completed: "完成", cancelled: "已取消", failed: "失敗", aborted: "已中止" }[s] ?? s
    );
  }
}
