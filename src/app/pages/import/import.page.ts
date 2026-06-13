import { Component, effect, inject, signal, untracked } from "@angular/core";
import { FormsModule } from "@angular/forms";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";

import { EtlStateService } from "../../services/etl-state.service";
import { LogService } from "../../services/log.service";
import {
  ConnectionConfig,
  TableInfo,
  TauriService,
  errorMessage,
} from "../../services/tauri.service";
import { WorkspaceService } from "../../services/workspace.service";

/** 顯示記憶體警示的行數閾值（開發計畫 §2.2.2：calamine 整檔載入）。 */
const LARGE_FILE_ROWS = 500_000;

@Component({
  selector: "app-import",
  imports: [FormsModule],
  templateUrl: "./import.page.html",
})
export class ImportPage {
  private readonly tauri = inject(TauriService);
  private readonly router = inject(Router);
  private readonly log = inject(LogService);
  readonly ws = inject(WorkspaceService);
  readonly state = inject(EtlStateService);

  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  /** 資料庫來源：可選資料表清單 */
  readonly tables = signal<TableInfo[]>([]);
  /** 資料庫來源：自訂 SQL 模式 */
  readonly useCustomSql = signal(false);
  private lastLoadedConnId: string | null = null;

  constructor() {
    // 頂部工具列選擇「來源」連線時自動載入（檔案連線載入預覽；資料庫連線列資料表）
    effect(() => {
      const conn = this.ws.sourceConnection();
      untracked(() => {
        if (conn && conn.id !== this.lastLoadedConnId) {
          this.lastLoadedConnId = conn.id;
          if (conn.kind === "file") {
            void this.loadFromConnection(conn);
          } else {
            void this.loadFromDbConnection(conn);
          }
        }
      });
    });
  }

  private async loadFromConnection(conn: ConnectionConfig): Promise<void> {
    this.state.resetSource();
    this.tables.set([]);
    this.useCustomSql.set(false);
    this.state.sourcePath.set(conn.database);
    this.state.encoding.set(conn.encoding ?? null);
    this.state.hasHeader.set(conn.hasHeader ?? true);
    this.error.set(null);
    this.loading.set(true);
    try {
      const sheets = await this.tauri.listSheets(conn.database);
      this.state.sheets.set(sheets);
      this.state.sheet.set(conn.sheet || sheets[0] || "");
      await this.loadPreview();
      this.log.info("匯入", `已載入來源連線「${conn.name}」`);
    } catch (e) {
      this.error.set(errorMessage(e));
      this.log.error("匯入", errorMessage(e));
    } finally {
      this.loading.set(false);
    }
  }

  private async loadFromDbConnection(conn: ConnectionConfig): Promise<void> {
    this.state.resetSource();
    this.state.sourceKind.set("database");
    this.state.dbConnId.set(conn.id);
    this.tables.set([]);
    this.useCustomSql.set(false);
    this.error.set(null);
    this.loading.set(true);
    try {
      this.tables.set(await this.tauri.getTables(conn.id));
      this.log.info(
        "匯入",
        `已連線資料庫來源「${conn.name}」，請選擇資料表或改用自訂 SQL`,
      );
    } catch (e) {
      this.error.set(errorMessage(e));
      this.log.error("匯入", errorMessage(e));
    } finally {
      this.loading.set(false);
    }
  }

  get isDbSource(): boolean {
    return this.state.sourceKind() === "database";
  }

  tableKey(t: TableInfo): string {
    return t.schema ? `${t.schema}.${t.name}` : t.name;
  }

  async onDbTableChange(table: string): Promise<void> {
    this.state.dbTable.set(table);
    if (!table) {
      this.state.preview.set(null);
      this.state.dbQuery.set("");
      return;
    }
    await this.loadDbPreview(table, null);
  }

  onCustomSqlToggle(on: boolean): void {
    this.useCustomSql.set(on);
    if (on) {
      this.state.dbTable.set("");
      this.state.preview.set(null);
      this.state.dbQuery.set("");
    }
  }

  async runCustomSql(): Promise<void> {
    const sql = this.state.dbCustomSql().trim();
    if (!sql) {
      return;
    }
    await this.loadDbPreview(null, sql);
  }

  private async loadDbPreview(table: string | null, query: string | null): Promise<void> {
    const connId = this.state.dbConnId();
    if (!connId) {
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const p = await this.tauri.readDbSourcePreview(connId, table, query);
      this.state.dbQuery.set(p.query);
      this.state.preview.set({
        columns: p.columns,
        rows: p.rows,
        totalRows: p.rows.length,
        encoding: null,
      });
      this.log.info(
        "匯入",
        `查詢預覽 ${p.rows.length} 行（${p.columns.length} 欄）`,
      );
    } catch (e) {
      this.error.set(errorMessage(e));
      this.state.preview.set(null);
      this.log.error("匯入", errorMessage(e));
    } finally {
      this.loading.set(false);
    }
  }

  readonly largeFileRows = LARGE_FILE_ROWS;
  readonly encodings = [
    { value: null, label: "自動偵測" },
    { value: "UTF-8", label: "UTF-8" },
    { value: "Big5", label: "Big5（繁中）" },
    { value: "UTF-16LE", label: "UTF-16 LE" },
    { value: "GBK", label: "GBK（簡中）" },
  ];

  get isCsv(): boolean {
    const p = this.state.sourcePath();
    return !!p && /\.(csv|tsv|txt)$/i.test(p);
  }

  fileName(): string {
    const p = this.state.sourcePath();
    return p ? (p.split(/[/\\]/).pop() ?? p) : "";
  }

  async pickFile(): Promise<void> {
    const path = await open({
      multiple: false,
      filters: [
        { name: "資料檔", extensions: ["xlsx", "xls", "xlsb", "ods", "csv", "tsv", "txt"] },
      ],
    });
    if (typeof path !== "string") {
      return;
    }
    this.state.resetSource();
    this.tables.set([]);
    this.useCustomSql.set(false);
    this.state.sourcePath.set(path);
    this.error.set(null);
    this.loading.set(true);
    try {
      const sheets = await this.tauri.listSheets(path);
      this.state.sheets.set(sheets);
      this.state.sheet.set(sheets[0] ?? "");
      await this.loadPreview();
      const p = this.state.preview();
      this.log.info(
        "匯入",
        `已載入 ${this.fileName()}（${p?.totalRows.toLocaleString() ?? "?"} 行${p?.encoding ? "，編碼 " + p.encoding : ""}）`,
      );
    } catch (e) {
      this.error.set(errorMessage(e));
      this.log.error("匯入", errorMessage(e));
    } finally {
      this.loading.set(false);
    }
  }

  async loadPreview(): Promise<void> {
    const path = this.state.sourcePath();
    const sheet = this.state.sheet();
    if (!path || !sheet) {
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const preview = await this.tauri.readPreview(
        path,
        sheet,
        this.state.hasHeader(),
        this.state.encoding(),
      );
      this.state.preview.set(preview);
    } catch (e) {
      this.error.set(errorMessage(e));
      this.state.preview.set(null);
      this.log.error("匯入", errorMessage(e));
    } finally {
      this.loading.set(false);
    }
  }

  async onSheetChange(sheet: string): Promise<void> {
    this.state.sheet.set(sheet);
    await this.loadPreview();
  }

  async onHeaderChange(hasHeader: boolean): Promise<void> {
    this.state.hasHeader.set(hasHeader);
    await this.loadPreview();
  }

  async onEncodingChange(encoding: string | null): Promise<void> {
    this.state.encoding.set(encoding);
    await this.loadPreview();
  }

  typeLabel(t: string | null): string {
    if (!t) {
      return "未定";
    }
    return (
      {
        integer: "整數",
        float: "浮點",
        text: "文字",
        bool: "布林",
        datetime: "日期時間",
        date: "日期",
      }[t] ?? t
    );
  }

  next(): void {
    this.router.navigate(["/works"]);
  }
}
