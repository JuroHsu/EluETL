import { Component, OnInit, inject, signal } from "@angular/core";
import {
  FormControl,
  FormGroup,
  ReactiveFormsModule,
  Validators,
} from "@angular/forms";

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

  readonly form = new FormGroup({
    name: new FormControl("", { nonNullable: true, validators: [Validators.required] }),
    kind: new FormControl<DbKind>("postgres", { nonNullable: true }),
    host: new FormControl("localhost", { nonNullable: true }),
    port: new FormControl<number | null>(null),
    database: new FormControl("", { nonNullable: true, validators: [Validators.required] }),
    username: new FormControl("", { nonNullable: true }),
    password: new FormControl("", { nonNullable: true }),
    trustServerCertificate: new FormControl(false, { nonNullable: true }),
  });

  async ngOnInit(): Promise<void> {
    try {
      await this.ws.reload();
    } catch (e) {
      this.result.set({ ok: false, message: errorMessage(e) });
    }
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
    });
  }

  newConnection(): void {
    this.editingId.set(null);
    this.result.set(null);
    this.form.reset({ kind: "postgres", host: "localhost" });
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
      this.result.set({ ok: true, message: "連線成功" });
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
      this.result.set({ ok: true, message: "已儲存（密碼存於系統金鑰圈）" });
      this.log.info("連線", `${config.name}：已儲存`);
      await this.ws.reload();
      if (!this.ws.activeConnId()) {
        this.ws.activeConnId.set(config.id);
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
      await this.ws.reload();
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
    }[kind];
  }
}
