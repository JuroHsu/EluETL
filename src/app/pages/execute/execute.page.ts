import { Component, computed, inject, signal } from "@angular/core";

import { EtlStateService } from "../../services/etl-state.service";
import { LogService } from "../../services/log.service";
import {
  EtlProgress,
  EtlSummary,
  TauriService,
  errorMessage,
} from "../../services/tauri.service";
import { WorkspaceService } from "../../services/workspace.service";

@Component({
  selector: "app-execute",
  templateUrl: "./execute.page.html",
})
export class ExecutePage {
  private readonly tauri = inject(TauriService);
  private readonly log = inject(LogService);
  private readonly ws = inject(WorkspaceService);
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
    await this.runJob(false);
  }

  async resume(): Promise<void> {
    await this.runJob(true);
  }

  private async runJob(resume: boolean): Promise<void> {
    const job = this.state.job();
    if (!job || this.running()) {
      return;
    }
    this.running.set(true);
    this.error.set(null);
    this.progress.set(null);
    this.state.summary.set(null);
    this.log.info(
      "ETL",
      `${resume ? "續跑" : "開始"}：${this.fileName()} → ${job.targetTable}`,
    );

    let lastLoggedBatch = -1;
    const onProgress = (p: EtlProgress) => {
      this.progress.set(p);
      this.ws.running.set(p);
      if (p.phase === "load" && p.batch !== lastLoggedBatch && p.batch > 0) {
        lastLoggedBatch = p.batch;
        this.log.info(
          "ETL",
          `批次 ${p.batch}/${p.totalBatches} — 成功 ${p.successRows.toLocaleString()}${p.errorRows ? `，錯誤 ${p.errorRows.toLocaleString()}` : ""}`,
        );
      }
    };

    try {
      const summary = resume
        ? await this.tauri.resumeEtl(job.jobId, onProgress)
        : await this.tauri.executeEtl(job, onProgress);
      this.state.summary.set(summary);
      this.logSummary(summary);
    } catch (e) {
      this.error.set(errorMessage(e));
      this.log.error("ETL", errorMessage(e));
    } finally {
      this.running.set(false);
      this.ws.running.set(null);
    }
  }

  private logSummary(s: EtlSummary): void {
    const base = `${this.statusLabel(s.status)} — 成功 ${s.successRows.toLocaleString()}/${s.totalRows.toLocaleString()} 行，錯誤 ${s.errorRows.toLocaleString()}，耗時 ${(s.elapsedMs / 1000).toFixed(1)}s`;
    if (s.status === "completed" && s.errorRows === 0) {
      this.log.success("ETL", base);
    } else if (s.status === "completed") {
      this.log.warn("ETL", base);
    } else {
      this.log.error("ETL", `${base}${s.failure ? " — " + s.failure : ""}`);
    }
  }

  async cancel(): Promise<void> {
    const job = this.state.job();
    if (job) {
      await this.tauri.cancelEtl(job.jobId);
      this.log.warn("ETL", "已送出取消請求（當前批次完成後停止）");
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
