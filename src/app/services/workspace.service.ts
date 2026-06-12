import { Injectable, computed, inject, signal } from "@angular/core";

import {
  ConnectionConfig,
  EtlProgress,
  TauriService,
} from "./tauri.service";

/**
 * 全域工作區狀態：已儲存連線清單、頂部工具列的「來源 / 目標」選擇、
 * 狀態列的執行中資訊與目標連線健康狀態。
 */
@Injectable({ providedIn: "root" })
export class WorkspaceService {
  private readonly tauri = inject(TauriService);

  readonly connections = signal<ConnectionConfig[]>([]);
  /** 來源連線（檔案類型；null = 由「匯入資料」頁手動載入） */
  readonly sourceConnId = signal<string | null>(null);
  /** 目標連線（資料庫類型） */
  readonly targetConnId = signal<string | null>(null);
  /** 執行中的 ETL 進度（null = 閒置），驅動狀態列 */
  readonly running = signal<EtlProgress | null>(null);
  /** 目標連線的健康狀態（狀態列指示燈） */
  readonly connStatus = signal<"none" | "connecting" | "connected" | "error">("none");

  readonly fileConnections = computed(() =>
    this.connections().filter((c) => c.kind === "file"),
  );

  readonly dbConnections = computed(() =>
    this.connections().filter((c) => c.kind !== "file"),
  );

  readonly sourceConnection = computed(() => {
    const id = this.sourceConnId();
    return this.connections().find((c) => c.id === id) ?? null;
  });

  readonly targetConnection = computed(() => {
    const id = this.targetConnId();
    return this.connections().find((c) => c.id === id) ?? null;
  });

  async reload(): Promise<void> {
    const list = await this.tauri.listConnections();
    this.connections.set(list);
    if (this.targetConnId() && !this.dbConnections().some((c) => c.id === this.targetConnId())) {
      this.targetConnId.set(null);
    }
    if (this.sourceConnId() && !this.fileConnections().some((c) => c.id === this.sourceConnId())) {
      this.sourceConnId.set(null);
    }
    const dbs = this.dbConnections();
    if (!this.targetConnId() && dbs.length === 1) {
      this.targetConnId.set(dbs[0].id);
    }
  }

  /** ping 目標連線並更新指示燈（連線切換時呼叫）。 */
  async pingActive(): Promise<void> {
    const id = this.targetConnId();
    if (!id) {
      this.connStatus.set("none");
      return;
    }
    this.connStatus.set("connecting");
    try {
      await this.tauri.pingConnection(id);
      if (this.targetConnId() === id) {
        this.connStatus.set("connected");
      }
    } catch {
      if (this.targetConnId() === id) {
        this.connStatus.set("error");
      }
    }
  }
}
