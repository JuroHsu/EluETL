import { Injectable } from "@angular/core";
import { invoke } from "@tauri-apps/api/core";

export type DbKind = "sqlserver" | "postgres" | "mysql" | "sqlite";

/** 對應 Rust `ConnectionConfig`（serde camelCase）。密碼不在此結構中。 */
export interface ConnectionConfig {
  id: string;
  name: string;
  kind: DbKind;
  host: string;
  port: number | null;
  database: string;
  username: string;
  trustServerCertificate: boolean;
}

export interface TableInfo {
  schema: string | null;
  name: string;
}

export interface ColumnInfo {
  name: string;
  dbType: string;
  nullable: boolean;
  ordinal: number;
}

/** 對應 Rust `EluEtlError` 的序列化格式。 */
export interface ApiError {
  code: string;
  message: string;
}

/**
 * 所有 Tauri IPC 呼叫的統一入口。
 * 元件不得直接 import `invoke`，以便測試時以 mock 替換本服務。
 */
@Injectable({ providedIn: "root" })
export class TauriService {
  greet(name: string): Promise<string> {
    return invoke<string>("greet", { name });
  }

  /** 測試連線；成功後後端會以 ConnectionId 快取驅動實例。 */
  testConnection(config: ConnectionConfig, password: string): Promise<void> {
    return invoke<void>("test_connection", { config, password });
  }

  getTables(connId: string): Promise<TableInfo[]> {
    return invoke<TableInfo[]>("get_tables", { connId });
  }

  getColumns(connId: string, table: string): Promise<ColumnInfo[]> {
    return invoke<ColumnInfo[]>("get_columns", { connId, table });
  }
}
