import { Injectable, computed, inject, signal } from "@angular/core";

import {
  ConnectionConfig,
  EtlProgress,
  TauriService,
} from "./tauri.service";

/**
 * 全域工作區狀態：已儲存連線清單、頂部工具列選擇的「使用中連線」、
 * 狀態列的執行中資訊。
 */
@Injectable({ providedIn: "root" })
export class WorkspaceService {
  private readonly tauri = inject(TauriService);

  readonly connections = signal<ConnectionConfig[]>([]);
  readonly activeConnId = signal<string | null>(null);
  /** 執行中的 ETL 進度（null = 閒置），驅動狀態列 */
  readonly running = signal<EtlProgress | null>(null);
  /** 使用中連線的健康狀態（狀態列指示燈） */
  readonly connStatus = signal<"none" | "connecting" | "connected" | "error">("none");

  readonly activeConnection = computed(() => {
    const id = this.activeConnId();
    return this.connections().find((c) => c.id === id) ?? null;
  });

  async reload(): Promise<void> {
    const list = await this.tauri.listConnections();
    this.connections.set(list);
    const active = this.activeConnId();
    if (active && !list.some((c) => c.id === active)) {
      this.activeConnId.set(null);
    }
    if (!this.activeConnId() && list.length === 1) {
      this.activeConnId.set(list[0].id);
    }
  }

  /** ping 使用中連線並更新指示燈（連線切換時呼叫）。 */
  async pingActive(): Promise<void> {
    const id = this.activeConnId();
    if (!id) {
      this.connStatus.set("none");
      return;
    }
    this.connStatus.set("connecting");
    try {
      await this.tauri.pingConnection(id);
      if (this.activeConnId() === id) {
        this.connStatus.set("connected");
      }
    } catch {
      if (this.activeConnId() === id) {
        this.connStatus.set("error");
      }
    }
  }
}
