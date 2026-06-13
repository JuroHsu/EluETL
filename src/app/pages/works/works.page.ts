import {
  AfterViewInit,
  Component,
  ElementRef,
  OnDestroy,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { StreamLanguage } from "@codemirror/language";
import { oneDark } from "@codemirror/theme-one-dark";
import { open, save } from "@tauri-apps/plugin-dialog";
import { EditorView, basicSetup } from "codemirror";

import { EtlStateService } from "../../services/etl-state.service";
import { LogService } from "../../services/log.service";
import { OutputService } from "../../services/output.service";
import {
  ColumnInfo,
  ScriptIssue,
  ScriptModel,
  ScriptWorkModel,
  TableInfo,
  TauriService,
  errorMessage,
} from "../../services/tauri.service";
import { WorkspaceService } from "../../services/workspace.service";

const SAMPLE = `-- 遷移作業範例：以 email 比對既有帳號，將外部身分寫入對應表
-- SOURCE / TARGET 標頭由上方工具列的「來源 / 目標」自動帶入；
-- 也可手寫 SOURCE = FILE(...) / CONNECTION('名稱') 覆寫（密碼一律不入檔）
-- Gen.XXX 為產生器：GUID / ULID / Date / DateTime / SHA256（整列雜湊）等
WORK 'EluCloudAccount綁定EnterId' {
  If [SOURCE].[userPrincipalName] == [dbo].[DirectoryAccounts].[Email]
  [dbo].[ExternalIdentityMappings]
  ADD {
     [Id] = Gen.ULID
    ,[AccountId] = [dbo].[DirectoryAccounts].[Id]
    ,[ExternalId] = [SOURCE].[id]
    ,[ExternalSystemType] = N'MICROSOFT_ENTRA_ID'
    ,[Label] = N'MICROSOFT_ENTRA_ID: {[dbo].[DirectoryAccounts].[DisplayName]}'
  }
}
GO
`;

/** 「生成」值來源的產生器選項（與 Rust GenKind::label 一致）。 */
const GENERATORS: { value: string; label: string }[] = [
  { value: "GUID", label: "GUID" },
  { value: "GUID(Text)", label: "GUID(Text)" },
  { value: "ULID", label: "ULID" },
  { value: "Date", label: "Date" },
  { value: "DateTime", label: "DateTime" },
  { value: "DateTimeOffset", label: "DateTimeOffset" },
  { value: "Date(Text)", label: "Date(Text)" },
  { value: "DateTime(Text)", label: "DateTime(Text)" },
  { value: "DateTimeOffset(Text)", label: "DateTimeOffset(Text)" },
  { value: "SHA256", label: "SHA256（整列雜湊）" },
  { value: "SHA512", label: "SHA512（整列雜湊）" },
  { value: "MD5", label: "MD5（整列雜湊）" },
];

/** ETL DSL 語法高亮（關鍵字 / [識別字] / 字串 / 數字 / 註解）。 */
const etlLanguage = StreamLanguage.define({
  startState: () => ({ inBlockComment: false }),
  token(stream, state: { inBlockComment: boolean }) {
    // /// … /// 多行註解（可跨行）
    if (state.inBlockComment) {
      if (stream.match(/^.*?\/\/\//)) state.inBlockComment = false;
      else stream.skipToEnd();
      return "comment";
    }
    if (stream.match(/^\/\/\//)) {
      if (!stream.match(/^.*?\/\/\//)) {
        stream.skipToEnd();
        state.inBlockComment = true;
      }
      return "comment";
    }
    if (stream.match(/^(--|\/\/).*/)) return "comment";
    if (stream.match(/^N?'([^']|'')*'/i)) return "string";
    if (stream.match(/^\[[^\]\n]*\]/)) return "variableName";
    if (stream.match(/^Gen\.\w+(\(\s*Text\s*\))?/i)) return "keyword";
    if (stream.match(/^(IF|ADD|GO|WORK|NULL|TRUE|FALSE|SOURCE|TARGET|FILE|CONNECTION|TYPE|PATH|SHEET|ENCODING|HEADER|TABLE|QUERY)\b/i)) {
      return "keyword";
    }
    if (stream.match(/^\d+(\.\d+)?/)) return "number";
    if (stream.match(/^(==|=|\{|\}|\(|\)|,|\.)/)) return "operator";
    stream.next();
    return null;
  },
});

/** 執行作業的單一欄位指派列。 */
interface AssignRow {
  targetColumn: string;
  /** 值來源：來源欄位 / 比對表欄位 / 常值 / 生成 / 合成欄位 */
  kind: "source" | "lookup" | "literal" | "gen" | "concat";
  /** source/lookup = 欄位名稱；literal = 字面值原文；gen = 產生器名稱；
   *  concat = 字串模板（如 N'前綴: {[SOURCE].[Name]}'，{…} 內為插值運算式） */
  value: string;
}

/** 視覺編輯的工作項目（對應一個 WORK 區塊）。 */
interface WorkItem {
  name: string;
  useCondition: boolean;
  condSourceColumn: string;
  /** 比對資料表（如 dbo.DirectoryAccounts） */
  condTable: string;
  condColumn: string;
  targetTable: string;
  assigns: AssignRow[];
}

@Component({
  selector: "app-works",
  imports: [FormsModule],
  templateUrl: "./works.page.html",
})
export class WorksPage implements AfterViewInit, OnDestroy {
  private readonly tauri = inject(TauriService);
  private readonly log = inject(LogService);
  readonly ws = inject(WorkspaceService);
  readonly state = inject(EtlStateService);
  /** 診斷 / 進度 / 結果統一寫入底部輸出面板（分頁呈現） */
  readonly output = inject(OutputService);

  private readonly editorHost = viewChild.required<ElementRef<HTMLElement>>("editorHost");
  private view: EditorView | null = null;

  /** 文字編輯模式（false = 三欄視覺編輯） */
  readonly textMode = signal(false);
  readonly works = signal<WorkItem[]>([]);
  readonly selected = signal(-1);

  readonly tables = signal<TableInfo[]>([]);
  readonly targetColumns = signal<ColumnInfo[]>([]);
  readonly lookupColumns = signal<ColumnInfo[]>([]);
  private readonly colCache = new Map<string, ColumnInfo[]>();

  /** 目前開啟的 .etl 檔案路徑（null = 未命名） */
  readonly currentFile = signal<string | null>(null);
  private jobId: string | null = null;

  readonly canRun = computed(() => !this.output.running());
  readonly generators = GENERATORS;

  constructor() {
    // 頂部工具列切換目標連線時重載資料表清單
    effect(() => {
      const connId = this.ws.targetConnId();
      untracked(() => void this.loadTables(connId));
    });
    // 來源 / 目標選擇或匯入狀態變更時，同步文字模式編輯器的 SOURCE / TARGET 標頭行
    effect(() => {
      this.textMode();
      this.ws.sourceConnId();
      this.ws.targetConnId();
      this.state.sourceKind();
      this.state.sourcePath();
      this.state.sheet();
      this.state.encoding();
      this.state.hasHeader();
      this.state.dbConnId();
      this.state.dbTable();
      this.state.dbQuery();
      this.state.dbCustomSql();
      untracked(() => this.syncHeaderInEditor());
    });
  }

  // ---- 生命週期 ----

  ngAfterViewInit(): void {
    const doc = this.state.scriptText() || SAMPLE;
    this.view = new EditorView({
      doc,
      extensions: [
        basicSetup,
        etlLanguage,
        oneDark,
        EditorView.theme({
          "&": { height: "100%", fontSize: "13px", backgroundColor: "#1e1e1e" },
          ".cm-gutters": { backgroundColor: "#1e1e1e" },
          // body 全域 cursor: default 會被繼承，這裡顯式還原文字編輯游標（I-beam）
          ".cm-content": { cursor: "text" },
          ".cm-scroller": { cursor: "text" },
        }),
      ],
      parent: this.editorHost().nativeElement,
    });
    // 初始進視覺模式；解析失敗則落回文字模式並顯示診斷
    void this.parseIntoModel(doc).then((ok) => this.textMode.set(!ok));
  }

  ngOnDestroy(): void {
    this.state.scriptText.set(this.currentText());
    this.view?.destroy();
    this.view = null;
  }

  // ---- 文字 ⇄ 視覺 ----

  private editorText(): string {
    return this.view?.state.doc.toString() ?? "";
  }

  private setEditorText(text: string): void {
    this.view?.dispatch({
      changes: { from: 0, to: this.view.state.doc.length, insert: text },
    });
  }

  /** 目前腳本內容：文字模式取編輯器，視覺模式由模型產生。 */
  currentText(): string {
    return this.textMode() ? this.editorText() : this.generateScript();
  }

  async toggleTextMode(): Promise<void> {
    this.output.checkMessage.set(null);
    if (this.textMode()) {
      if (await this.parseIntoModel(this.editorText())) {
        this.textMode.set(false);
      } else {
        // 解析失敗無法切到視覺模式：把錯誤帶到診斷分頁讓使用者看到原因
        this.output.show("diagnostics");
      }
    } else {
      this.setEditorText(this.generateScript());
      this.textMode.set(true);
    }
  }

  private async parseIntoModel(text: string): Promise<boolean> {
    try {
      const m = await this.tauri.parseEtlScript(text);
      const warnings = this.syncHeaderToWorkspace(m);
      this.works.set(this.modelToWorks(m));
      this.selected.set(this.works().length ? 0 : -1);
      this.output.issues.set(warnings);
      void this.refreshColumnsForSelected();
      return true;
    } catch (e) {
      this.output.issues.set([{ line: 0, message: errorMessage(e) }]);
      return false;
    }
  }

  /** 以名稱查找已儲存連線（與後端 get_connection_by_name 同樣不分大小寫、忽略前後空白）。 */
  private findConnByName(name: string) {
    const target = name.trim().toLowerCase();
    return this.ws.connections().find((c) => c.name.trim().toLowerCase() === target) ?? null;
  }

  /**
   * 腳本標頭 → 工具列：SOURCE / TARGET 的 CONNECTION 名稱回頭設定上方選擇器，
   * inline FILE 來源寫入匯入狀態（來源選擇器 = 手動）。找不到的連線名稱以警告回報。
   */
  private syncHeaderToWorkspace(m: ScriptModel): ScriptIssue[] {
    const warnings: ScriptIssue[] = [];
    if (m.targetConnection) {
      const hit = this.findConnByName(m.targetConnection);
      if (hit) {
        this.ws.targetConnId.set(hit.id);
      } else {
        warnings.push({
          line: 0,
          message: `找不到目標連線「${m.targetConnection}」，已改用上方工具列的目標選擇`,
        });
      }
    }
    const src = m.source;
    if (!src) {
      return warnings;
    }
    if (src.type === "connection") {
      const hit = this.findConnByName(src.name);
      if (!hit) {
        warnings.push({
          line: 0,
          message: `找不到來源連線「${src.name}」，已改用上方工具列的來源選擇`,
        });
        return warnings;
      }
      this.ws.sourceConnId.set(hit.id);
      if (hit.kind !== "file") {
        this.state.sourceKind.set("database");
        this.state.dbConnId.set(hit.id);
        this.state.dbTable.set(src.table ?? "");
        if (src.query) {
          this.state.dbCustomSql.set(src.query);
          this.state.dbQuery.set(src.query);
        } else {
          this.state.dbQuery.set("");
        }
      }
    } else {
      // inline FILE：來源選擇器切回手動，檔案參數寫入匯入狀態
      this.ws.sourceConnId.set(null);
      this.state.sourceKind.set("file");
      this.state.sourcePath.set(src.path);
      this.state.sheet.set(src.sheet ?? "");
      this.state.encoding.set(src.encoding);
      this.state.hasHeader.set(src.hasHeader ?? true);
    }
    return warnings;
  }

  private modelToWorks(m: ScriptModel): WorkItem[] {
    return m.works.map((w, i) => this.workFromModel(w, i));
  }

  private workFromModel(w: ScriptWorkModel, index: number): WorkItem {
    const lookupKey = w.condition
      ? w.condition.right.prefix.map((p) => p.toLowerCase()).join(".")
      : null;
    return {
      name: w.name ?? `作業 ${index + 1}`,
      useCondition: !!w.condition,
      condSourceColumn: w.condition?.left.column ?? "",
      condTable: w.condition?.right.prefix.join(".") ?? "",
      condColumn: w.condition?.right.column ?? "",
      targetTable: w.targetTable.join("."),
      assigns: w.assignments.map((a): AssignRow => {
        const v = a.value;
        switch (v.kind) {
          case "col": {
            const key = v.prefix.map((p) => p.toLowerCase()).join(".");
            if (lookupKey !== null && key === lookupKey) {
              return { targetColumn: a.targetColumn, kind: "lookup", value: v.column };
            }
            return { targetColumn: a.targetColumn, kind: "source", value: v.column };
          }
          case "text":
            return {
              targetColumn: a.targetColumn,
              kind: "literal",
              value: `N'${v.value.replace(/'/g, "''")}'`,
            };
          case "int":
          case "float":
            return { targetColumn: a.targetColumn, kind: "literal", value: String(v.value) };
          case "bool":
            return {
              targetColumn: a.targetColumn,
              kind: "literal",
              value: v.value ? "TRUE" : "FALSE",
            };
          case "null":
            return { targetColumn: a.targetColumn, kind: "literal", value: "NULL" };
          case "gen":
            return { targetColumn: a.targetColumn, kind: "gen", value: v.name };
          case "concat":
            return { targetColumn: a.targetColumn, kind: "concat", value: v.expr };
        }
      }),
    };
  }

  // ---- 腳本產生（視覺模型 → DSL 文字） ----

  private esc(s: string): string {
    return s.replace(/'/g, "''");
  }

  private tableRef(table: string): string {
    return table
      .split(".")
      .filter((p) => p)
      .map((p) => `[${p}]`)
      .join(".");
  }

  /** 常值輸入正規化：數字 / TRUE / FALSE / NULL / 已加引號者原樣，其餘包成 N'…'。 */
  private literalText(raw: string): string {
    const t = raw.trim();
    if (!t) {
      return "NULL";
    }
    if (/^-?\d+(\.\d+)?$/.test(t) || /^(TRUE|FALSE|NULL)$/i.test(t) || /^N?'[\s\S]*'$/i.test(t)) {
      return t;
    }
    return `N'${this.esc(t)}'`;
  }

  private valueText(w: WorkItem, a: AssignRow): string {
    switch (a.kind) {
      case "source":
        return `[SOURCE].[${a.value}]`;
      case "lookup":
        return `${this.tableRef(w.condTable)}.[${a.value}]`;
      case "literal":
        return this.literalText(a.value);
      case "gen":
        return `Gen.${a.value || "GUID"}`;
      case "concat":
        return a.value.trim() || "NULL";
    }
  }

  generateScript(): string {
    const lines: string[] = [];
    const src = this.effectiveSourceLine();
    if (src) {
      lines.push(src);
    }
    const tgt = this.effectiveTargetLine();
    if (tgt) {
      lines.push(tgt);
    }
    if (lines.length) {
      lines.push("");
    }
    for (const w of this.works()) {
      lines.push(`WORK '${this.esc(w.name)}' {`);
      if (w.useCondition && w.condSourceColumn && w.condTable && w.condColumn) {
        lines.push(
          `  If [SOURCE].[${w.condSourceColumn}] == ${this.tableRef(w.condTable)}.[${w.condColumn}]`,
        );
      }
      lines.push(`  ${this.tableRef(w.targetTable)}`);
      lines.push("  ADD {");
      w.assigns
        .filter((a) => a.targetColumn)
        .forEach((a, i) => {
          lines.push(`    ${i === 0 ? " " : ","}[${a.targetColumn}] = ${this.valueText(w, a)}`);
        });
      lines.push("  }");
      lines.push("}");
    }
    lines.push("GO");
    lines.push("");
    return lines.join("\n");
  }

  /**
   * SOURCE 標頭行：對應上方工具列的「來源」選擇。
   * 連線 → CONNECTION('名稱'[, TABLE/QUERY])；手動載入檔案 → FILE(...)；無來源 → null。
   */
  private effectiveSourceLine(): string | null {
    const conn = this.ws.sourceConnection();
    if (conn) {
      if (conn.kind === "file") {
        return `SOURCE = CONNECTION('${this.esc(conn.name)}')`;
      }
      // 資料庫來源：資料表 / SQL 由「匯入資料」頁選定（屬於同一連線時才帶入）
      if (this.state.dbConnId() === conn.id) {
        const table = this.state.dbTable();
        if (table) {
          return `SOURCE = CONNECTION('${this.esc(conn.name)}', TABLE='${this.esc(table)}')`;
        }
        const query = this.state.dbQuery() || this.state.dbCustomSql();
        if (query) {
          return `SOURCE = CONNECTION('${this.esc(conn.name)}', QUERY='${this.esc(query)}')`;
        }
      }
      return `SOURCE = CONNECTION('${this.esc(conn.name)}')`;
    }
    if (this.state.sourceKind() === "file") {
      const path = this.state.sourcePath();
      if (path) {
        const sheet = this.state.sheet();
        const encoding = this.state.encoding();
        const args = [
          `PATH='${this.esc(path)}'`,
          ...(sheet ? [`SHEET='${this.esc(sheet)}'`] : []),
          ...(encoding ? [`ENCODING='${this.esc(encoding)}'`] : []),
          `HEADER=${this.state.hasHeader() ? "TRUE" : "FALSE"}`,
        ];
        return `SOURCE = FILE(${args.join(", ")})`;
      }
    }
    return null;
  }

  /** TARGET 標頭行：對應上方工具列的「目標」選擇。 */
  private effectiveTargetLine(): string | null {
    const conn = this.ws.targetConnection();
    return conn ? `TARGET = CONNECTION('${this.esc(conn.name)}')` : null;
  }

  /**
   * 文字模式的標頭同步：剔除前導區的舊 SOURCE / TARGET 行，
   * 換上工具列對應的標頭。沒有權威值的部分保留原行（不吃掉手寫標頭）。
   */
  private spliceHeader(text: string): string {
    const src = this.effectiveSourceLine();
    const tgt = this.effectiveTargetLine();
    if (!src && !tgt) {
      return text;
    }
    const lines = text.split("\n");
    const leading: string[] = [];
    let oldSrc: string | null = null;
    let oldTgt: string | null = null;
    let rest = 0;
    for (; rest < lines.length; rest++) {
      const t = lines[rest].trim();
      if (/^SOURCE\s*=/i.test(t)) {
        oldSrc = lines[rest];
        continue;
      }
      if (/^TARGET\s*=/i.test(t)) {
        oldTgt = lines[rest];
        continue;
      }
      if (t === "" || t.startsWith("--") || t.startsWith("//")) {
        leading.push(lines[rest]);
        continue;
      }
      break;
    }
    while (leading.length && leading[leading.length - 1].trim() === "") {
      leading.pop();
    }
    const header = [src ?? oldSrc, tgt ?? oldTgt].filter((l): l is string => !!l);
    return [...leading, ...header, "", ...lines.slice(rest)].join("\n");
  }

  /** 工具列來源 / 目標變更時，讓文字模式編輯器中的標頭行跟著更新。 */
  private syncHeaderInEditor(): void {
    if (!this.view || !this.textMode()) {
      return;
    }
    const updated = this.spliceHeader(this.editorText());
    if (updated !== this.editorText()) {
      this.setEditorText(updated);
    }
  }

  // ---- 工作項目操作 ----

  selectedWork(): WorkItem | null {
    return this.works()[this.selected()] ?? null;
  }

  selectWork(i: number): void {
    this.selected.set(i);
    void this.refreshColumnsForSelected();
  }

  addWork(): void {
    const item: WorkItem = {
      name: `作業 ${this.works().length + 1}`,
      useCondition: false,
      condSourceColumn: "",
      condTable: "",
      condColumn: "",
      targetTable: "",
      assigns: [{ targetColumn: "", kind: "source", value: "" }],
    };
    this.works.update((a) => [...a, item]);
    this.selectWork(this.works().length - 1);
  }

  removeWork(i: number): void {
    this.works.update((a) => a.filter((_, idx) => idx !== i));
    const n = this.works().length;
    this.selected.set(Math.min(this.selected(), n - 1));
  }

  /** ngModel 直接改物件屬性；左欄名稱等清單顯示需重建陣列觸發更新。 */
  worksChanged(): void {
    this.works.update((a) => [...a]);
  }

  addAssign(w: WorkItem): void {
    w.assigns.push({ targetColumn: "", kind: "source", value: "" });
    this.worksChanged();
  }

  removeAssign(w: WorkItem, i: number): void {
    w.assigns.splice(i, 1);
    this.worksChanged();
  }

  /** 值來源切換：進「生成」給預設產生器；離開時清掉產生器名稱殘值。 */
  onAssignKindChange(a: AssignRow, kind: AssignRow["kind"]): void {
    a.kind = kind;
    const isGenName = GENERATORS.some((g) => g.value === a.value);
    if (kind === "gen" && !isGenName) {
      a.value = "GUID";
    } else if (kind !== "gen" && isGenName) {
      a.value = "";
    }
    this.worksChanged();
  }

  /** 以欄名（不分大小寫）自動對應來源欄位 → 目標表欄位。 */
  async autoMatch(): Promise<void> {
    const w = this.selectedWork();
    const preview = this.state.preview();
    if (!w || !preview || !w.targetTable) {
      return;
    }
    await this.refreshTargetColumns();
    const byName = new Map(this.targetColumns().map((c) => [c.name.toLowerCase(), c]));
    const rows: AssignRow[] = [];
    for (const c of preview.columns) {
      const hit = byName.get(c.name.toLowerCase());
      if (hit) {
        rows.push({ targetColumn: hit.name, kind: "source", value: c.name });
      }
    }
    if (rows.length) {
      w.assigns = rows;
      this.worksChanged();
      this.log.info("作業", `${w.targetTable}：自動對應 ${rows.length} 欄`);
    } else {
      this.log.warn("作業", "沒有名稱相符的欄位（請先於「匯入資料」載入來源）");
    }
  }

  // ---- 資料表 / 欄位中繼資料 ----

  private async loadTables(connId: string | null): Promise<void> {
    this.tables.set([]);
    if (!connId) {
      return;
    }
    try {
      this.tables.set(await this.tauri.getTables(connId));
    } catch (e) {
      this.log.error("作業", errorMessage(e));
    }
  }

  tableKey(t: TableInfo): string {
    return t.schema ? `${t.schema}.${t.name}` : t.name;
  }

  private async loadColumns(table: string): Promise<ColumnInfo[]> {
    const connId = this.ws.targetConnId();
    if (!connId || !table) {
      return [];
    }
    const key = `${connId}|${table.toLowerCase()}`;
    const hit = this.colCache.get(key);
    if (hit) {
      return hit;
    }
    try {
      const cols = await this.tauri.getColumns(connId, table);
      this.colCache.set(key, cols);
      return cols;
    } catch {
      return [];
    }
  }

  async refreshTargetColumns(): Promise<void> {
    this.targetColumns.set(await this.loadColumns(this.selectedWork()?.targetTable ?? ""));
  }

  async refreshLookupColumns(): Promise<void> {
    this.lookupColumns.set(await this.loadColumns(this.selectedWork()?.condTable ?? ""));
  }

  private async refreshColumnsForSelected(): Promise<void> {
    await Promise.all([this.refreshTargetColumns(), this.refreshLookupColumns()]);
  }

  sourceColumnNames(): string[] {
    return this.state.preview()?.columns.map((c) => c.name) ?? [];
  }

  // ---- 檔案 / 驗證 / 執行（沿用 ETL 腳本頁） ----

  etlFileName(): string {
    const p = this.currentFile();
    return p ? (p.split(/[/\\]/).pop() ?? p) : "未命名";
  }

  /** 腳本來源摘要（與 SOURCE 標頭一致：工具列連線優先，其次手動載入的來源）。 */
  scriptSourceLabel(): string | null {
    const conn = this.ws.sourceConnection();
    if (conn) {
      if (conn.kind === "file") {
        const file = conn.database.split(/[/\\]/).pop() ?? conn.database;
        return `${conn.name}（${file}）`;
      }
      const detail =
        this.state.dbConnId() === conn.id
          ? this.state.dbTable() || this.state.dbQuery() || this.state.dbCustomSql()
          : "";
      return detail ? `${conn.name}：${detail}` : `${conn.name}（請於「匯入資料」選擇資料表）`;
    }
    if (this.state.sourceKind() === "database") {
      const q = this.state.dbQuery();
      return q ? `DB：${q}` : null;
    }
    const p = this.state.sourcePath();
    return p ? `${p.split(/[/\\]/).pop() ?? p}（${this.state.sheet()}）` : null;
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
      this.setEditorText(content);
      this.currentFile.set(path);
      this.output.clearResult();
      this.output.checkMessage.set(null);
      const ok = await this.parseIntoModel(content);
      this.textMode.set(!ok);
      this.log.info("作業", `已開啟 ${this.etlFileName()}`);
    } catch (e) {
      this.log.error("作業", errorMessage(e));
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
      await this.tauri.saveEtlFile(path, this.currentText());
      this.currentFile.set(path);
      this.log.success("作業", `已儲存 ${this.etlFileName()}`);
    } catch (e) {
      this.log.error("作業", errorMessage(e));
    }
  }

  async insertSample(): Promise<void> {
    this.setEditorText(SAMPLE);
    this.output.clearResult();
    this.output.checkMessage.set(null);
    const ok = await this.parseIntoModel(SAMPLE);
    this.textMode.set(!ok);
  }

  async validate(): Promise<void> {
    this.output.show("diagnostics");
    this.output.checkMessage.set(null);
    this.output.issues.set([]);
    const cols = this.state.preview()?.columns.map((c) => c.name) ?? null;
    try {
      const check = await this.tauri.validateEtlScript(this.currentText(), cols);
      this.output.issues.set(check.issues);
      this.output.checkMessage.set(
        check.ok
          ? `語法正確（${check.statementCount} 個工作項目${cols ? "，來源欄位已核對" : "；尚未載入來源，僅檢查語法"}）`
          : null,
      );
    } catch (e) {
      this.output.issues.set([{ line: 0, message: errorMessage(e) }]);
    }
  }

  async run(): Promise<void> {
    if (this.output.running()) {
      return;
    }
    // 工作區選擇作為回退；腳本內的 SOURCE / TARGET 標頭優先（後端解析）
    const connId = this.ws.targetConnId();
    const isDb = this.state.sourceKind() === "database";
    const dbQuery = this.state.dbQuery();
    const sourcePath = isDb ? null : this.state.sourcePath();
    const sourceConnId = isDb && dbQuery ? this.state.dbConnId() : null;
    this.output.show("result");
    this.output.clearResult();
    this.output.running.set(true);
    this.jobId = crypto.randomUUID();
    this.log.info("作業", `開始執行 ${this.etlFileName()}`);

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
          sourceConnId,
          sourceQuery: sourceConnId ? dbQuery : null,
          batchSize: 5000,
          script: this.currentText(),
        },
        (p) => {
          this.output.progress.set(p);
          this.ws.running.set(p);
          if (p.phase === "load" && p.batch !== lastBatch && p.batch > 0) {
            lastBatch = p.batch;
            this.log.info(
              "作業",
              `批次 ${p.batch}/${p.totalBatches} — 寫入 ${p.successRows.toLocaleString()}`,
            );
          }
        },
      );
      this.output.summary.set(summary);
      const msg = `${this.output.statusLabel(summary.status)} — 寫入 ${summary.successRows.toLocaleString()} 行（來源 ${summary.totalRows.toLocaleString()} 行），錯誤 ${summary.errorRows.toLocaleString()}，耗時 ${(summary.elapsedMs / 1000).toFixed(1)}s`;
      if (summary.status === "completed" && summary.errorRows === 0) {
        this.log.success("作業", msg);
      } else if (summary.status === "completed") {
        this.log.warn("作業", msg);
      } else {
        this.log.error("作業", `${msg}${summary.failure ? " — " + summary.failure : ""}`);
      }
    } catch (e) {
      this.output.error.set(errorMessage(e));
      this.log.error("作業", errorMessage(e));
    } finally {
      this.output.running.set(false);
      this.ws.running.set(null);
    }
  }

  async cancel(): Promise<void> {
    if (this.jobId) {
      await this.tauri.cancelEtl(this.jobId);
      this.log.warn("作業", "已送出取消請求");
    }
  }
}
