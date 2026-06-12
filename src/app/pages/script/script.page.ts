import {
  AfterViewInit,
  Component,
  ElementRef,
  OnDestroy,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { StreamLanguage } from "@codemirror/language";
import { oneDark } from "@codemirror/theme-one-dark";
import { open, save } from "@tauri-apps/plugin-dialog";
import { EditorView, basicSetup } from "codemirror";

import { EtlStateService } from "../../services/etl-state.service";
import { LogService } from "../../services/log.service";
import {
  EtlProgress,
  EtlSummary,
  ScriptIssue,
  TauriService,
  errorMessage,
} from "../../services/tauri.service";
import { WorkspaceService } from "../../services/workspace.service";

const SAMPLE = `-- ETL 腳本範例：以 email 比對既有帳號，將外部身分寫入對應表
-- SOURCE / TARGET 可省略：省略時使用上方工具列選擇的來源與目標
-- 安全原則：TARGET 以連線名稱引用，密碼留在系統金鑰圈，不寫入腳本
SOURCE = FILE(TYPE=CSV, PATH='C:\\data\\users.csv', ENCODING='Big5', HEADER=TRUE)
TARGET = CONNECTION('正式環境 ERP')

If [SOURCE].email == [dbo].[Account].email
[dbo].[ExternalIdentityMappings] ADD
{
 AccountId = [dbo].[Account].Id
,ExternalId = [SOURCE].Id
,ExternalSystemType = N'MICROSOFT_ENTRA_ID'
}
GO
`;

/** ETL DSL 語法高亮（關鍵字 / [識別字] / 字串 / 數字 / 註解）。 */
const etlLanguage = StreamLanguage.define({
  token(stream) {
    if (stream.match(/^--.*/)) return "comment";
    if (stream.match(/^N?'([^']|'')*'/i)) return "string";
    if (stream.match(/^\[[^\]\n]*\]/)) return "variableName";
    if (stream.match(/^(IF|ADD|GO|NULL|TRUE|FALSE|SOURCE|TARGET|FILE|CONNECTION|TYPE|PATH|SHEET|ENCODING|HEADER)\b/i)) {
      return "keyword";
    }
    if (stream.match(/^\d+(\.\d+)?/)) return "number";
    if (stream.match(/^(==|=|\{|\}|\(|\)|,|\.)/)) return "operator";
    stream.next();
    return null;
  },
});

@Component({
  selector: "app-script",
  templateUrl: "./script.page.html",
})
export class ScriptPage implements AfterViewInit, OnDestroy {
  private readonly tauri = inject(TauriService);
  private readonly log = inject(LogService);
  readonly ws = inject(WorkspaceService);
  readonly state = inject(EtlStateService);

  private readonly editorHost = viewChild.required<ElementRef<HTMLElement>>("editorHost");
  private view: EditorView | null = null;

  readonly issues = signal<ScriptIssue[]>([]);
  readonly checkMessage = signal<string | null>(null);
  readonly running = signal(false);
  readonly progress = signal<EtlProgress | null>(null);
  readonly summary = signal<EtlSummary | null>(null);
  readonly error = signal<string | null>(null);
  /** 目前開啟的 .etl 檔案路徑（null = 未命名） */
  readonly currentFile = signal<string | null>(null);
  private jobId: string | null = null;

  readonly canRun = computed(() => !this.running());

  fileName(): string {
    const p = this.state.sourcePath();
    return p ? (p.split(/[/\\]/).pop() ?? p) : "";
  }

  etlFileName(): string {
    const p = this.currentFile();
    return p ? (p.split(/[/\\]/).pop() ?? p) : "未命名";
  }

  async openFile(): Promise<void> {
    const path = await open({
      multiple: false,
      filters: [{ name: "ETL 腳本", extensions: ["etl"] }],
    });
    if (typeof path !== "string") {
      return;
    }
    try {
      const content = await this.tauri.loadEtlFile(path);
      this.view?.dispatch({
        changes: { from: 0, to: this.view.state.doc.length, insert: content },
      });
      this.currentFile.set(path);
      this.summary.set(null);
      this.issues.set([]);
      this.checkMessage.set(null);
      this.log.info("腳本", `已開啟 ${this.etlFileName()}`);
    } catch (e) {
      this.error.set(errorMessage(e));
    }
  }

  async saveFile(): Promise<void> {
    let path = this.currentFile();
    if (!path) {
      const picked = await save({
        filters: [{ name: "ETL 腳本", extensions: ["etl"] }],
        defaultPath: "未命名.etl",
      });
      if (typeof picked !== "string") {
        return;
      }
      path = picked;
    }
    try {
      await this.tauri.saveEtlFile(path, this.text());
      this.currentFile.set(path);
      this.log.success("腳本", `已儲存 ${this.etlFileName()}`);
    } catch (e) {
      this.error.set(errorMessage(e));
    }
  }

  ngAfterViewInit(): void {
    this.view = new EditorView({
      doc: this.state.scriptText() || SAMPLE,
      extensions: [
        basicSetup,
        etlLanguage,
        oneDark,
        EditorView.theme({
          "&": { height: "100%", fontSize: "13px", backgroundColor: "#1e1e1e" },
          ".cm-gutters": { backgroundColor: "#1e1e1e" },
        }),
      ],
      parent: this.editorHost().nativeElement,
    });
  }

  ngOnDestroy(): void {
    if (this.view) {
      this.state.scriptText.set(this.view.state.doc.toString());
      this.view.destroy();
    }
  }

  private text(): string {
    return this.view?.state.doc.toString() ?? "";
  }

  insertSample(): void {
    this.view?.dispatch({
      changes: { from: 0, to: this.view.state.doc.length, insert: SAMPLE },
    });
  }

  async validate(): Promise<void> {
    this.checkMessage.set(null);
    this.issues.set([]);
    const cols = this.state.preview()?.columns.map((c) => c.name) ?? null;
    try {
      const check = await this.tauri.validateEtlScript(this.text(), cols);
      this.issues.set(check.issues);
      this.checkMessage.set(
        check.ok
          ? `語法正確（${check.statementCount} 段陳述式${cols ? "，來源欄位已核對" : "；尚未載入來源檔，僅檢查語法"}）`
          : null,
      );
    } catch (e) {
      this.issues.set([{ line: 0, message: errorMessage(e) }]);
    }
  }

  async run(): Promise<void> {
    if (this.running()) {
      return;
    }
    // 工作區選擇作為回退；腳本內的 SOURCE / TARGET 標頭優先（後端解析）
    const connId = this.ws.targetConnId();
    const sourcePath = this.state.sourcePath();
    this.running.set(true);
    this.error.set(null);
    this.summary.set(null);
    this.progress.set(null);
    this.jobId = crypto.randomUUID();
    this.log.info("腳本", `開始執行 ${this.etlFileName()}`);

    let lastBatch = -1;
    try {
      const summary = await this.tauri.executeEtlScript(
        {
          jobId: this.jobId,
          connId,
          sourcePath,
          sheet: sourcePath ? this.state.sheet() : null,
          hasHeader: sourcePath ? this.state.hasHeader() : null,
          encoding: sourcePath ? this.state.encoding() : null,
          batchSize: 5000,
          script: this.text(),
        },
        (p) => {
          this.progress.set(p);
          this.ws.running.set(p);
          if (p.phase === "load" && p.batch !== lastBatch && p.batch > 0) {
            lastBatch = p.batch;
            this.log.info(
              "腳本",
              `批次 ${p.batch}/${p.totalBatches} — 寫入 ${p.successRows.toLocaleString()}`,
            );
          }
        },
      );
      this.summary.set(summary);
      const msg = `${this.statusLabel(summary.status)} — 寫入 ${summary.successRows.toLocaleString()} 行（來源 ${summary.totalRows.toLocaleString()} 行），錯誤 ${summary.errorRows.toLocaleString()}，耗時 ${(summary.elapsedMs / 1000).toFixed(1)}s`;
      if (summary.status === "completed" && summary.errorRows === 0) {
        this.log.success("腳本", msg);
      } else if (summary.status === "completed") {
        this.log.warn("腳本", msg);
      } else {
        this.log.error("腳本", `${msg}${summary.failure ? " — " + summary.failure : ""}`);
      }
    } catch (e) {
      this.error.set(errorMessage(e));
      this.log.error("腳本", errorMessage(e));
    } finally {
      this.running.set(false);
      this.ws.running.set(null);
    }
  }

  async cancel(): Promise<void> {
    if (this.jobId) {
      await this.tauri.cancelEtl(this.jobId);
      this.log.warn("腳本", "已送出取消請求");
    }
  }

  statusLabel(s: EtlSummary["status"]): string {
    return (
      { completed: "完成", cancelled: "已取消", failed: "失敗", aborted: "已中止" }[s] ?? s
    );
  }
}
