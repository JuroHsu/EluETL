import { Component, inject, signal } from "@angular/core";
import { FormsModule } from "@angular/forms";
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";

import { EtlStateService } from "../../services/etl-state.service";
import { LogService } from "../../services/log.service";
import { TauriService, errorMessage } from "../../services/tauri.service";

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
  readonly state = inject(EtlStateService);

  readonly loading = signal(false);
  readonly error = signal<string | null>(null);

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
    this.router.navigate(["/mapping"]);
  }
}
