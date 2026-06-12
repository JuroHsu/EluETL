import { Injectable, signal } from "@angular/core";

export type LogLevel = "info" | "success" | "warn" | "error";

export interface LogEntry {
  time: Date;
  level: LogLevel;
  source: string;
  message: string;
}

const MAX_ENTRIES = 500;

/** 底部輸出面板的執行日誌（僅 UI 顯示；完整審計日誌在 Rust 端 tracing）。 */
@Injectable({ providedIn: "root" })
export class LogService {
  readonly entries = signal<LogEntry[]>([]);

  add(level: LogLevel, source: string, message: string): void {
    this.entries.update((list) => {
      const next = [...list, { time: new Date(), level, source, message }];
      return next.length > MAX_ENTRIES ? next.slice(-MAX_ENTRIES) : next;
    });
  }

  info(source: string, message: string): void {
    this.add("info", source, message);
  }

  success(source: string, message: string): void {
    this.add("success", source, message);
  }

  warn(source: string, message: string): void {
    this.add("warn", source, message);
  }

  error(source: string, message: string): void {
    this.add("error", source, message);
  }

  clear(): void {
    this.entries.set([]);
  }
}
