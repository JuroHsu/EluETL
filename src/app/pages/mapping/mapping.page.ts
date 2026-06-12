import { Component, OnInit, inject, signal } from "@angular/core";
import { FormsModule } from "@angular/forms";
import { Router } from "@angular/router";

import { EtlStateService } from "../../services/etl-state.service";
import {
  ColumnInfo,
  ConnectionConfig,
  DataType,
  ErrorPolicy,
  EtlJobConfig,
  MappingRule,
  TableInfo,
  TauriService,
  WriteMode,
  errorMessage,
} from "../../services/tauri.service";

/** 單一來源欄的對應編輯列。 */
interface RuleRow {
  sourceIndex: number;
  sourceName: string;
  inferredType: DataType | null;
  targetColumn: string; // "" = 略過
  targetType: DataType;
  allowNull: boolean;
}

@Component({
  selector: "app-mapping",
  imports: [FormsModule],
  templateUrl: "./mapping.page.html",
})
export class MappingPage implements OnInit {
  private readonly tauri = inject(TauriService);
  private readonly router = inject(Router);
  readonly state = inject(EtlStateService);

  readonly connections = signal<ConnectionConfig[]>([]);
  readonly tables = signal<TableInfo[]>([]);
  readonly targetColumns = signal<ColumnInfo[]>([]);
  readonly rows = signal<RuleRow[]>([]);
  readonly error = signal<string | null>(null);
  readonly loading = signal(false);

  connId = "";
  table = "";
  writeMode: "batchCommit" | "allOrNothing" = "batchCommit";
  errorPolicy: "skipAndReport" | "abortOnFirst" | "abortOnErrorRate" = "skipAndReport";
  maxErrorPercent = 10;
  batchSize = 5000;

  readonly dataTypes: { value: DataType; label: string }[] = [
    { value: "integer", label: "整數" },
    { value: "float", label: "浮點" },
    { value: "text", label: "文字" },
    { value: "bool", label: "布林" },
    { value: "datetime", label: "日期時間" },
    { value: "date", label: "日期" },
  ];

  async ngOnInit(): Promise<void> {
    try {
      this.connections.set(await this.tauri.listConnections());
    } catch (e) {
      this.error.set(errorMessage(e));
    }
    const preview = this.state.preview();
    if (preview) {
      this.rows.set(
        preview.columns.map((c) => ({
          sourceIndex: c.index,
          sourceName: c.name,
          inferredType: c.inferredType,
          targetColumn: "",
          targetType: c.inferredType ?? "text",
          allowNull: true,
        })),
      );
    }
  }

  get hasSource(): boolean {
    return this.state.preview() !== null;
  }

  async onConnChange(): Promise<void> {
    this.table = "";
    this.tables.set([]);
    this.targetColumns.set([]);
    if (!this.connId) {
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      this.tables.set(await this.tauri.getTables(this.connId));
    } catch (e) {
      this.error.set(errorMessage(e));
    } finally {
      this.loading.set(false);
    }
  }

  tableKey(t: TableInfo): string {
    return t.schema ? `${t.schema}.${t.name}` : t.name;
  }

  async onTableChange(): Promise<void> {
    this.targetColumns.set([]);
    if (!this.connId || !this.table) {
      return;
    }
    this.loading.set(true);
    this.error.set(null);
    try {
      const cols = await this.tauri.getColumns(this.connId, this.table);
      this.targetColumns.set(cols);
      this.autoMatch(cols);
    } catch (e) {
      this.error.set(errorMessage(e));
    } finally {
      this.loading.set(false);
    }
  }

  /** 以欄名（不分大小寫）自動對應，並依 DB 型別建議轉換型別。 */
  private autoMatch(cols: ColumnInfo[]): void {
    const byName = new Map(cols.map((c) => [c.name.toLowerCase(), c]));
    this.rows.update((rows) =>
      rows.map((r) => {
        const hit = byName.get(r.sourceName.toLowerCase());
        if (!hit) {
          return { ...r, targetColumn: "" };
        }
        return {
          ...r,
          targetColumn: hit.name,
          targetType: suggestType(hit.dbType) ?? r.inferredType ?? "text",
          allowNull: hit.nullable,
        };
      }),
    );
  }

  onTargetPicked(row: RuleRow): void {
    const col = this.targetColumns().find((c) => c.name === row.targetColumn);
    if (col) {
      row.targetType = suggestType(col.dbType) ?? row.inferredType ?? "text";
      row.allowNull = col.nullable;
    }
  }

  mappedCount(): number {
    return this.rows().filter((r) => r.targetColumn).length;
  }

  canProceed(): boolean {
    return !!this.connId && !!this.table && this.mappedCount() > 0;
  }

  proceed(): void {
    const preview = this.state.preview();
    const sourcePath = this.state.sourcePath();
    if (!preview || !sourcePath || !this.canProceed()) {
      return;
    }
    const rules: MappingRule[] = this.rows()
      .filter((r) => r.targetColumn)
      .map((r) => ({
        sourceIndex: r.sourceIndex,
        sourceName: r.sourceName,
        targetColumn: r.targetColumn,
        targetType: r.targetType,
        nullPolicy: r.allowNull ? "allow" : "error",
      }));

    const writeMode: WriteMode = { mode: this.writeMode };
    const errorPolicy: ErrorPolicy =
      this.errorPolicy === "abortOnErrorRate"
        ? { policy: "abortOnErrorRate", maxPercent: this.maxErrorPercent }
        : { policy: this.errorPolicy };

    const job: EtlJobConfig = {
      jobId: crypto.randomUUID(),
      connId: this.connId,
      sourcePath,
      sheet: this.state.sheet(),
      hasHeader: this.state.hasHeader(),
      encoding: this.state.encoding(),
      targetTable: this.table,
      rules,
      writeMode,
      errorPolicy,
      batchSize: this.batchSize,
    };
    this.state.job.set(job);
    this.state.summary.set(null);
    this.router.navigate(["/execute"]);
  }
}

/** DB 原生型別 → 建議轉換型別。 */
function suggestType(dbType: string): DataType | null {
  const t = dbType.toLowerCase();
  if (/bool|^bit$/.test(t)) return "bool";
  if (/int|year|serial/.test(t)) return "integer";
  if (/decimal|numeric|real|double|float|money/.test(t)) return "float";
  if (/datetime|timestamp/.test(t)) return "datetime";
  if (/^date$/.test(t)) return "date";
  if (/char|text|clob|uuid|json|xml|enum/.test(t)) return "text";
  return null;
}
