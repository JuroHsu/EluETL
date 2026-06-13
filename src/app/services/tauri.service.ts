import { Injectable } from "@angular/core";
import { Channel, invoke } from "@tauri-apps/api/core";

export type DbKind = "sqlserver" | "postgres" | "mysql" | "sqlite" | "db2" | "file";
export type DataType = "integer" | "float" | "text" | "bool" | "datetime" | "date";
export type NullPolicy = "allow" | "error";

/** 對應 Rust `ConnectionConfig`（serde camelCase）。密碼不在此結構中。 */
export interface ConnectionConfig {
  id: string;
  name: string;
  kind: DbKind;
  host: string;
  port: number | null;
  /** 檔案連線時為檔案路徑 */
  database: string;
  username: string;
  trustServerCertificate: boolean;
  /** 檔案連線：工作表（null = 第一個） */
  sheet?: string | null;
  /** 檔案連線：CSV 編碼覆寫（null = 自動偵測） */
  encoding?: string | null;
  /** 檔案連線：首列為欄名（null = true） */
  hasHeader?: boolean | null;
}

export interface TableInfo {
  schema: string | null;
  name: string;
}

/** 對應 Rust `Db2DriverStatus`：IBM DB2 驅動就緒狀態。 */
export interface Db2DriverStatus {
  /** 真正可連線（本版本含 db2 feature 且系統偵測到 IBM 驅動）。 */
  available: boolean;
  /** 本版本是否以 db2 feature 編譯。 */
  featureBuilt: boolean;
  /** 是否在系統環境偵測到 IBM Data Server Driver。 */
  driverPresent: boolean;
  /** 人類可讀說明。 */
  message: string;
  /** 驅動下載頁。 */
  downloadUrl: string;
}

export interface ColumnInfo {
  name: string;
  dbType: string;
  nullable: boolean;
  ordinal: number;
}

export interface ColumnPreview {
  index: number;
  name: string;
  /** null = 取樣全為 NULL，型別未定 */
  inferredType: DataType | null;
}

export interface SourcePreview {
  columns: ColumnPreview[];
  rows: unknown[][];
  totalRows: number;
  encoding: string | null;
}

export interface MappingRule {
  sourceIndex: number;
  sourceName: string;
  targetColumn: string;
  targetType: DataType;
  nullPolicy: NullPolicy;
}

export type WriteMode = { mode: "batchCommit" } | { mode: "allOrNothing" };

export type ErrorPolicy =
  | { policy: "skipAndReport" }
  | { policy: "abortOnFirst" }
  | { policy: "abortOnErrorRate"; maxPercent: number };

/** ETL 來源：檔案（Excel / CSV）或資料庫查詢（對應 Rust `SourceSpec`）。 */
export type SourceSpec =
  | { type: "file"; path: string; sheet: string; hasHeader: boolean; encoding: string | null }
  | { type: "database"; connId: string; query: string };

export interface EtlJobConfig {
  jobId: string;
  connId: string;
  source: SourceSpec;
  targetTable: string;
  rules: MappingRule[];
  writeMode: WriteMode;
  errorPolicy: ErrorPolicy;
  batchSize: number;
}

export interface EtlProgress {
  jobId: string;
  phase: "read" | "transform" | "load";
  batch: number;
  totalBatches: number;
  successRows: number;
  errorRows: number;
}

export interface RowError {
  row: number;
  column: string;
  reason: string;
}

export interface EtlSummary {
  jobId: string;
  status: "completed" | "cancelled" | "failed" | "aborted";
  totalRows: number;
  successRows: number;
  errorRows: number;
  elapsedMs: number;
  failure: string | null;
  errors: RowError[];
}

export interface QueryPreview {
  columns: string[];
  rows: unknown[][];
}

export interface ScriptIssue {
  line: number;
  message: string;
}

export interface ScriptCheck {
  ok: boolean;
  statementCount: number;
  issues: ScriptIssue[];
}

// ---- 結構化腳本模型（「遷移作業」頁視覺編輯器；對應 Rust ScriptModel）----

export interface ColRefModel {
  prefix: string[];
  column: string;
}

export type ExprModel =
  | { kind: "col"; prefix: string[]; column: string }
  | { kind: "text"; value: string }
  | { kind: "int"; value: number }
  | { kind: "float"; value: number }
  | { kind: "bool"; value: boolean }
  | { kind: "null" }
  | { kind: "gen"; name: string }
  | { kind: "concat"; expr: string };

export interface ScriptWorkModel {
  name: string | null;
  condition: { left: ColRefModel; right: ColRefModel } | null;
  targetTable: string[];
  assignments: { targetColumn: string; value: ExprModel }[];
}

export type ScriptSourceModel =
  | {
      type: "file";
      path: string;
      sheet: string | null;
      encoding: string | null;
      hasHeader: boolean | null;
    }
  | { type: "connection"; name: string; table: string | null; query: string | null };

export interface ScriptModel {
  source: ScriptSourceModel | null;
  targetConnection: string | null;
  works: ScriptWorkModel[];
}

/** 腳本任務參數：來源/目標可省略（腳本 SOURCE/TARGET 標頭優先）。
 *  工作區來源為資料庫時改傳 sourceConnId + sourceQuery。 */
export interface ScriptJobParams {
  jobId: string;
  connId: string | null;
  sourcePath: string | null;
  sheet: string | null;
  hasHeader: boolean | null;
  encoding: string | null;
  sourceConnId: string | null;
  sourceQuery: string | null;
  batchSize: number;
  script: string;
}

/** 資料庫來源預覽：query 為實際採用的 SQL（存入任務設定 / 腳本產生）。 */
export interface DbSourcePreview {
  columns: ColumnPreview[];
  rows: unknown[][];
  query: string;
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

  // ---- 連線管理 ----

  testConnection(config: ConnectionConfig, password: string): Promise<void> {
    return invoke<void>("test_connection", { config, password });
  }

  saveConnection(config: ConnectionConfig, password: string | null): Promise<void> {
    return invoke<void>("save_connection", { config, password });
  }

  listConnections(): Promise<ConnectionConfig[]> {
    return invoke<ConnectionConfig[]>("list_connections");
  }

  /** 偵測 IBM DB2 驅動就緒狀態（選擇 DB2 連線類型時呼叫）。 */
  checkDb2Driver(): Promise<Db2DriverStatus> {
    return invoke<Db2DriverStatus>("check_db2_driver");
  }

  deleteConnection(connId: string): Promise<void> {
    return invoke<void>("delete_connection", { connId });
  }

  /** 驗證使用中連線是否可用（狀態列指示燈）。 */
  pingConnection(connId: string): Promise<void> {
    return invoke<void>("ping_connection", { connId });
  }

  getTables(connId: string): Promise<TableInfo[]> {
    return invoke<TableInfo[]>("get_tables", { connId });
  }

  getColumns(connId: string, table: string): Promise<ColumnInfo[]> {
    return invoke<ColumnInfo[]>("get_columns", { connId, table });
  }

  queryPreview(connId: string, sql: string): Promise<QueryPreview> {
    return invoke<QueryPreview>("query_preview", { connId, sql });
  }

  /** 查詢結果匯出 xlsx（資料全程在 Rust 端流動）。回傳列數。 */
  exportQueryToExcel(connId: string, sql: string, outputPath: string): Promise<number> {
    return invoke<number>("export_query_to_excel", { connId, sql, outputPath });
  }

  // ---- 來源檔 ----

  listSheets(path: string): Promise<string[]> {
    return invoke<string[]>("list_sheets", { path });
  }

  readPreview(
    path: string,
    sheet: string,
    hasHeader: boolean,
    encoding: string | null,
  ): Promise<SourcePreview> {
    return invoke<SourcePreview>("read_preview", { path, sheet, hasHeader, encoding });
  }

  /** 資料庫來源預覽：指定資料表（SELECT *）或自訂 SQL（擇一）。 */
  readDbSourcePreview(
    connId: string,
    table: string | null,
    query: string | null,
  ): Promise<DbSourcePreview> {
    return invoke<DbSourcePreview>("read_db_source_preview", { connId, table, query });
  }

  // ---- ETL ----

  executeEtl(
    job: EtlJobConfig,
    onProgress: (p: EtlProgress) => void,
  ): Promise<EtlSummary> {
    const progress = new Channel<EtlProgress>();
    progress.onmessage = onProgress;
    return invoke<EtlSummary>("execute_etl", { job, onProgress: progress });
  }

  cancelEtl(jobId: string): Promise<boolean> {
    return invoke<boolean>("cancel_etl", { jobId });
  }

  resumeEtl(
    jobId: string,
    onProgress: (p: EtlProgress) => void,
  ): Promise<EtlSummary> {
    const progress = new Channel<EtlProgress>();
    progress.onmessage = onProgress;
    return invoke<EtlSummary>("resume_etl", { jobId, onProgress: progress });
  }

  // ---- ETL 腳本 ----

  validateEtlScript(
    script: string,
    sourceColumns: string[] | null,
  ): Promise<ScriptCheck> {
    return invoke<ScriptCheck>("validate_etl_script", { script, sourceColumns });
  }

  /** 解析腳本為結構化模型（遷移作業頁的視覺編輯器）。 */
  parseEtlScript(script: string): Promise<ScriptModel> {
    return invoke<ScriptModel>("parse_etl_script", { script });
  }

  executeEtlScript(
    params: ScriptJobParams,
    onProgress: (p: EtlProgress) => void,
  ): Promise<EtlSummary> {
    const progress = new Channel<EtlProgress>();
    progress.onmessage = onProgress;
    return invoke<EtlSummary>("execute_etl_script", { params, onProgress: progress });
  }

  loadEtlFile(path: string): Promise<string> {
    return invoke<string>("load_etl_file", { path });
  }

  saveEtlFile(path: string, content: string): Promise<void> {
    return invoke<void>("save_etl_file", { path, content });
  }
}

/** 將 IPC 例外轉為人類可讀訊息。 */
export function errorMessage(e: unknown): string {
  const err = e as ApiError;
  return err?.message ?? String(e);
}
