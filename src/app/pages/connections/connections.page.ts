import { Component, inject, signal } from "@angular/core";
import {
  FormControl,
  FormGroup,
  ReactiveFormsModule,
  Validators,
} from "@angular/forms";

import {
  ApiError,
  ConnectionConfig,
  DbKind,
  TauriService,
} from "../../services/tauri.service";

@Component({
  selector: "app-connections",
  imports: [ReactiveFormsModule],
  templateUrl: "./connections.page.html",
})
export class ConnectionsPage {
  private readonly tauri = inject(TauriService);

  readonly testing = signal(false);
  readonly result = signal<{ ok: boolean; message: string } | null>(null);

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

  get kind(): DbKind {
    return this.form.controls.kind.value;
  }

  get isSqlite(): boolean {
    return this.kind === "sqlite";
  }

  get isSqlServer(): boolean {
    return this.kind === "sqlserver";
  }

  async testConnection(): Promise<void> {
    if (this.form.invalid || this.testing()) {
      this.form.markAllAsTouched();
      return;
    }
    this.testing.set(true);
    this.result.set(null);

    const v = this.form.getRawValue();
    const config: ConnectionConfig = {
      id: crypto.randomUUID(),
      name: v.name,
      kind: v.kind,
      host: v.host,
      port: v.port,
      database: v.database,
      username: v.username,
      trustServerCertificate: v.trustServerCertificate,
    };

    try {
      await this.tauri.testConnection(config, v.password);
      this.result.set({ ok: true, message: "連線成功" });
    } catch (e) {
      const err = e as ApiError;
      this.result.set({
        ok: false,
        message: err?.message ?? String(e),
      });
    } finally {
      this.testing.set(false);
    }
  }
}
