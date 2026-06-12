import { Component, computed, inject, signal } from "@angular/core";

import { EtlStateService } from "../../services/etl-state.service";
import {
  EtlProgress,
  EtlSummary,
  TauriService,
  errorMessage,
} from "../../services/tauri.service";

@Component({
  selector: "app-execute",
  templateUrl: "./execute.page.html",
})
export class ExecutePage {
  private readonly tauri = inject(TauriService);
  readonly state = inject(EtlStateService);

  readonly running = signal(false);
  readonly progress = signal<EtlProgress | null>(null);
  readonly error = signal<string | null>(null);

  readonly percent = computed(() => {
    const p = this.progress();
    if (!p || p.totalBatches === 0) {
      return 0;
    }
    return Math.min(100, Math.round((p.batch / p.totalBatches) * 100));
  });

  readonly canResume = computed(() => {
    const s = this.state.summary();
    return (
      !this.running() &&
      !!s &&
      (s.status === "cancelled" || s.status === "failed") &&
      this.state.job()?.writeMode.mode === "batchCommit"
    );
  });

  fileName(): string {
    const p = this.state.job()?.sourcePath ?? "";
    return p.split(/[/\\]/).pop() ?? p;
  }

  async start(): Promise<void> {
    const job = this.state.job();
    if (!job || this.running()) {
      return;
    }
    this.prepare();
    try {
      const summary = await this.tauri.executeEtl(job, (p) => this.progress.set(p));
      this.state.summary.set(summary);
    } catch (e) {
      this.error.set(errorMessage(e));
    } finally {
      this.running.set(false);
    }
  }

  async resume(): Promise<void> {
    const job = this.state.job();
    if (!job || this.running()) {
      return;
    }
    this.prepare();
    try {
      const summary = await this.tauri.resumeEtl(job.jobId, (p) => this.progress.set(p));
      this.state.summary.set(summary);
    } catch (e) {
      this.error.set(errorMessage(e));
    } finally {
      this.running.set(false);
    }
  }

  private prepare(): void {
    this.running.set(true);
    this.error.set(null);
    this.progress.set(null);
    this.state.summary.set(null);
  }

  async cancel(): Promise<void> {
    const job = this.state.job();
    if (job) {
      await this.tauri.cancelEtl(job.jobId);
    }
  }

  statusLabel(s: EtlSummary["status"]): string {
    return (
      {
        completed: "完成",
        cancelled: "已取消",
        failed: "失敗",
        aborted: "已中止",
      }[s] ?? s
    );
  }

  phaseLabel(p: string): string {
    return { read: "讀取來源", transform: "型別轉換", load: "寫入資料庫" }[p] ?? p;
  }
}
