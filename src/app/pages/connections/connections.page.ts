import { Component, OnInit, inject, signal } from "@angular/core";
import {
  FormControl,
  FormGroup,
  ReactiveFormsModule,
  Validators,
} from "@angular/forms";
import { open } from "@tauri-apps/plugin-dialog";

import { LogService } from "../../services/log.service";
import {
  ConnectionConfig,
  DbKind,
  TauriService,
  errorMessage,
} from "../../services/tauri.service";
import { WorkspaceService } from "../../services/workspace.service";

@Component({
  selector: "app-connections",
  imports: [ReactiveFormsModule],
  templateUrl: "./connections.page.html",
})
export class ConnectionsPage implements OnInit {
  private readonly tauri = inject(TauriService);
  private readonly log = inject(LogService);
  readonly ws = inject(WorkspaceService);

  readonly busy = signal(false);
  readonly result = signal<{ ok: boolean; message: string } | null>(null);
  /** 編輯中的既有連線 id（null = 新增） */
  readonly editingId = signal<string | null>(null);

  readonly encodings = [
    { value: "", label: "自動偵測" },
    { value: "UTF-8", label: "UTF-8" },
    { value: "Big5", label: "Big5（繁中）" },
    { value: "UTF-16LE", label: "UTF-16 LE" },
    { value: "GBK", label: "GBK（簡中）" },
  ];

  readonly form = new FormGroup({
    name: new FormControl("", { nonNullable: true, validators: [Validators.required] }),
    kind: new FormControl<DbKind>("postgres", { nonNullable: true }),
    host: new FormControl("localhost", { nonNullable: true }),
    port: new FormControl<number | null>(null),
    database: new FormControl("", { nonNullable: true, validators: [Validators.required] }),
    username: new FormControl("", { nonNullable: true }),
    password: new FormControl("", { nonNullable: true }),
    trustServerCertificate: new FormControl(false, { nonNullable: true }),
    sheet: new FormControl("", { nonNullable: true }),
    encoding: new FormControl("", { nonNullable: true }),
    hasHeader: new FormControl(true, { nonNullable: true }),
  });

  async ngOnInit(): Promise<void> {
    await this.reload();
  }

  get kind(): DbKind {
    return this.form.controls.kind.value;
  }

  get isSqlite(): boolean {
    return this.kind === "sqlite";
  }

  get isSqlServer(): boolean {
    return this.kind === "sqlserver";
  }

  get isFile(): boolean {
    return this.kind === "file";
  }

  get isDbWithAuth(): boolean {
    return !this.isSqlite && !this.isFile;
  }

  async reload(): Promise<void> {
    try {
      await this.ws.reload();
    } catch (e) {
      this.result.set({ ok: false, message: errorMessage(e) });
    }
  }

  async pickFile(): Promise<void> {
    const path = await open({
      multiple: false,
      filters: [
        { name: "資料檔", extensions: ["xlsx", "xls", "xlsb", "ods", "csv", "tsv", "txt"] },
      ],
    });
    if (typeof path === "string") {
      this.form.controls.database.setValue(path);
      if (!this.form.controls.name.value) {
        this.form.controls.name.setValue(path.split(/[/\\]/).pop() ?? path);
      }
    }
  }

  edit(conn: ConnectionConfig): void {
    this.editingId.set(conn.id);
    this.result.set(null);
    this.form.patchValue({
      name: conn.name,
      kind: conn.kind,
      host: conn.host,
      port: conn.port,
      database: conn.database,
      username: conn.username,
      password: "",
      trustServerCertificate: conn.trustServerCertificate,
      sheet: conn.sheet ?? "",
      encoding: conn.encoding ?? "",
      hasHeader: conn.hasHeader ?? true,
    });
  }

  newConnection(): void {
    this.editingId.set(null);
    this.result.set(null);
    this.form.reset({ kind: "postgres", host: "localhost", hasHeader: true });
  }

  private buildConfig(): ConnectionConfig {
    const v = this.form.getRawValue();
    return {
      id: this.editingId() ?? crypto.randomUUID(),
      name: v.name,
      kind: v.kind,
      host: v.host,
      port: v.port,
      database: v.database,
      username: v.username,
      trustServerCertificate: v.trustServerCertificate,
      sheet: this.isFile && v.sheet ? v.sheet : null,
      encoding: this.isFile && v.encoding ? v.encoding : null,
      hasHeader: this.isFile ? v.hasHeader : null,
    };
  }

  async testConnection(): Promise<void> {
    if (this.form.invalid || this.busy()) {
      this.form.markAllAsTouched();
      return;
    }
    this.busy.set(true);
    this.result.set(null);
    const config = this.buildConfig();
    try {
      await this.tauri.testConnection(config, this.form.controls.password.value);
      this.result.set({ ok: true, message: this.isFile ? "檔案可讀取" : "連線成功" });
      this.log.success("連線", `${config.name}：測試成功`);
    } catch (e) {
      this.result.set({ ok: false, message: errorMessage(e) });
      this.log.error("連線", `${config.name}：${errorMessage(e)}`);
    } finally {
      this.busy.set(false);
    }
  }

  /** 儲存連線：設定進 state.db；密碼（若有輸入）進 OS keychain。 */
  async save(): Promise<void> {
    if (this.form.invalid || this.busy()) {
      this.form.markAllAsTouched();
      return;
    }
    this.busy.set(true);
    this.result.set(null);
    try {
      const config = this.buildConfig();
      const pw = this.form.controls.password.value;
      await this.tauri.saveConnection(config, pw ? pw : null);
      this.editingId.set(config.id);
      this.result.set({
        ok: true,
        message: this.isFile ? "已儲存" : "已儲存（密碼存於系統金鑰圈）",
      });
      this.log.info("連線", `${config.name}：已儲存`);
      await this.reload();
      // 自動帶入頂部工具列：檔案 → 來源；資料庫 → 目標
      if (config.kind === "file") {
        if (!this.ws.sourceConnId()) {
          this.ws.sourceConnId.set(config.id);
        }
      } else if (!this.ws.targetConnId()) {
        this.ws.targetConnId.set(config.id);
      }
    } catch (e) {
      this.result.set({ ok: false, message: errorMessage(e) });
      this.log.error("連線", errorMessage(e));
    } finally {
      this.busy.set(false);
    }
  }

  async remove(conn: ConnectionConfig): Promise<void> {
    if (!confirm(`確定刪除連線「${conn.name}」？（金鑰圈中的密碼將一併刪除）`)) {
      return;
    }
    try {
      await this.tauri.deleteConnection(conn.id);
      this.log.info("連線", `${conn.name}：已刪除`);
      if (this.editingId() === conn.id) {
        this.newConnection();
      }
      await this.reload();
    } catch (e) {
      this.result.set({ ok: false, message: errorMessage(e) });
    }
  }

  kindLabel(kind: DbKind): string {
    return {
      sqlserver: "SQL Server",
      postgres: "PostgreSQL",
      mysql: "MySQL",
      sqlite: "SQLite",
      file: "檔案",
    }[kind];
  }
}
