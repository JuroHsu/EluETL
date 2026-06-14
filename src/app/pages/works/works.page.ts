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
import { NgTemplateOutlet } from "@angular/common";
import { FormsModule } from "@angular/forms";
import { StreamLanguage } from "@codemirror/language";
import { oneDark } from "@codemirror/theme-one-dark";
import { tags as t } from "@lezer/highlight";
import { open, save } from "@tauri-apps/plugin-dialog";
import { EditorView, basicSetup } from "codemirror";

import { EtlStateService } from "../../services/etl-state.service";
import { LogService } from "../../services/log.service";
import { OutputService } from "../../services/output.service";
import {
  ActionModel,
  ColumnInfo,
  CondRowModel,
  ConditionModel,
  ExprModel,
  JoinModel,
  ScriptIssue,
  ScriptModel,
  ScriptWorkModel,
  TableInfo,
  TauriService,
  errorMessage,
} from "../../services/tauri.service";
import { WorkspaceService } from "../../services/workspace.service";

const SAMPLE = `-- 遷移作業範例（DSL v0.2）：CSV/Entra → EluCloud（查表 + 新增）
-- SOURCE / TARGET 標頭由上方工具列自動帶入；也可手寫覆寫（密碼不入檔）
WORK 'EluCloudAccount綁定EnterId' {
  FROM entra   = SOURCE.[users]
  JOIN account = TARGET.[dbo].[DirectoryAccounts]
    ON (entra.[userPrincipalName] == account.[Email])

  INTO TARGET.[dbo].[ExternalIdentityMappings]
  ADD {
     [Id]                 = Gen.ULID
    ,[AccountId]          = account.[Id]
    ,[ExternalId]         = entra.[id]
    ,[ExternalSystemType] = N'MICROSOFT_ENTRA_ID'
    ,[Label]              = N'MICROSOFT_ENTRA_ID: {account.[DisplayName]}'
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

/** 比較運算子 + 空值 / 集合 / 樣式 / 區間（條件 / 篩選下拉）。 */
const COND_OPS = [
  "==",
  "!=",
  ">",
  "<",
  ">=",
  "<=",
  "IS EMPTY",
  "IS NOT EMPTY",
  "IN",
  "NOT IN",
  "LIKE",
  "NOT LIKE",
  "BETWEEN",
  "NOT BETWEEN",
];

/**
 * ETL DSL v0.2 語法高亮（StreamLanguage，含字串插值）。
 * 進入 N'…' 後遇 `{` 切到插值狀態，洞內欄位/產生器照常上色，`}` 切回；{{ }} 維持字面。
 */
type ETLState = { inBlockComment: boolean; inString: boolean; inInterp: boolean };

const KEYWORDS_SINGLE =
  /^(WORK|FROM|JOIN|WHERE|INTO|ON|MATCHED|MATCH|ADD|UPDATE|SKIP|DELETE|GO|SOURCE|TARGET|CONNECTION|FILE|TYPE|PATH|SHEET|ENCODING|HEADER|TABLE|QUERY|IS|EMPTY|NOT|IN|LIKE|BETWEEN|INNER|LEFT|JUDGE|EXECUTE|IF)\b/i;

const etlLanguage = StreamLanguage.define<ETLState>({
  startState: () => ({ inBlockComment: false, inString: false, inInterp: false }),

  token(stream, state) {
    // 0. 多行區塊註解 /// … ///（會跨行，必須最先處理）
    if (state.inBlockComment) {
      if (stream.match(/^.*?\/\/\//)) state.inBlockComment = false;
      else stream.skipToEnd();
      return "comment";
    }

    // 字串以單行為界：跨行未閉合則於行首重置，避免污染後續行
    if (stream.sol() && state.inString) {
      state.inString = false;
      state.inInterp = false;
    }

    // 1. 字串內部（含 {…} 插值）
    if (state.inString) {
      if (state.inInterp) {
        if (stream.match(/^\}/)) {
          state.inInterp = false;
          return "punctuation";
        }
        if (stream.match(/^Gen\.\w+(\s*\(\s*Text\s*\))?/i)) return "keyword";
        if (stream.match(/^\[[^\]\n]*\]/)) return "variableName";
        if (stream.match(/^[A-Za-z_]\w*/)) return "propertyName";
        if (stream.match(/^\d+(\.\d+)?/)) return "number";
        if (stream.match(/^(==|!=|<=|>=|&&|\|\||[=!<>])/)) return "operator";
        if (stream.match(/^[.,]/)) return "punctuation";
        stream.next();
        return null;
      }
      if (stream.match(/^''/)) return "string"; // '' 跳脫單引號（先於單一 ' 檢查）
      if (stream.match(/^'/)) {
        state.inString = false;
        return "string";
      }
      if (stream.match(/^(\{\{|\}\})/)) return "string"; // {{ }} 字面大括號
      if (stream.match(/^\{/)) {
        state.inInterp = true;
        return "punctuation";
      }
      if (stream.match(/^[^'{}]+/)) return "string";
      stream.next();
      return "string";
    }

    // 2. 單行註解
    if (stream.match(/^(--|\/\/).*/)) return "comment";

    // 3. 區塊註解起始
    if (stream.match(/^\/\/\//)) {
      if (!stream.match(/^.*?\/\/\//)) {
        stream.skipToEnd();
        state.inBlockComment = true;
      }
      return "comment";
    }

    // 4. 字串起始 N'…' / '…'
    if (stream.match(/^N?'/i)) {
      state.inString = true;
      return "string";
    }

    // 5. 字面值
    if (stream.match(/^\b(TRUE|FALSE)\b/i)) return "bool";
    if (stream.match(/^\bNULL\b/i)) return "null";
    if (stream.match(/^\d+(\.\d+)?/)) return "number";

    // 6. 產生器 Gen.*
    if (stream.match(/^Gen\.\w+(\s*\(\s*Text\s*\))?/i)) return "keyword";

    // 7. 多字關鍵字（必須排在單字之前）
    if (stream.match(/^IS\s+NOT\s+EMPTY\b/i)) return "keyword";
    if (stream.match(/^IS\s+EMPTY\b/i)) return "keyword";
    if (stream.match(/^NOT\s+MATCHED\b/i)) return "keyword";

    // 8. 單字關鍵字
    if (stream.match(KEYWORDS_SINGLE)) return "keyword";

    // 9. 中括號識別字 [dbo].[Table]
    if (stream.match(/^\[[^\]\n]*\]/)) return "variableName";

    // 10. 一般識別字 / 別名 / 成員（account、mapping、entra…）
    if (stream.match(/^[A-Za-z_]\w*/)) return "propertyName";

    // 11. 運算子（多字元優先）
    if (stream.match(/^(==|!=|<=|>=|&&|\|\||[=!<>])/)) return "operator";

    // 12. 標點 / 結構
    if (stream.match(/^[{}()\[\],.]/)) return "punctuation";

    stream.next();
    return null;
  },

  tokenTable: {
    bool: t.bool,
    null: t.null,
    punctuation: t.punctuation,
  },
});

// ---- 視覺編輯模型 ----

type ConnKind = "source" | "target";

/** 執行作業的單一欄位指派列。 */
interface AssignRow {
  targetColumn: string;
  /** 值來源：欄位（別名.欄位）/ 常值 / 生成 / 合成欄位 */
  kind: "field" | "literal" | "gen" | "concat";
  /** field：別名 */
  alias: string;
  /** field：欄位名；gen/literal/concat 不用 */
  column: string;
  /** gen = 產生器名；literal = 字面值原文；concat = 字串模板 */
  value: string;
}

/** JOIN ON 的一條等式：來源側 == 查表側。 */
interface JoinOnRow {
  leftAlias: string;
  leftColumn: string;
  rightColumn: string;
}

/** 關聯表（查表 / lookup join）。 */
interface JoinItem {
  alias: string;
  conn: ConnKind;
  table: string;
  on: JoinOnRow[];
  policy: "inner" | "left";
}

/** 篩選（WHERE）的一條件。op 決定用到哪些值欄位。 */
interface FilterRow {
  alias: string;
  column: string;
  op: string; // COND_OPS
  /** 比較值 / LIKE 樣式 */
  value: string;
  /** IN 清單（逗號分隔） */
  values: string;
  /** BETWEEN 下界 */
  low: string;
  /** BETWEEN 上界 */
  high: string;
}

/** 合併鍵（merge ON）的一條件：目標欄位 op 來源值。 */
interface MergeOnRow {
  targetColumn: string;
  op: string;
  rhsKind: "field" | "literal";
  rhsAlias: string;
  rhsColumn: string;
  rhsLiteral: string;
}

/** 視覺編輯的工作項目（對應一個 WORK 區塊）。 */
interface WorkItem {
  name: string;
  fromAlias: string;
  fromConn: ConnKind;
  fromTable: string;
  joins: JoinItem[];
  /** WHERE：simple 時用 filters 編輯；否則（含 OR/NOT）用 whereRaw 唯讀 */
  whereSimple: boolean;
  filters: FilterRow[];
  whereRaw: string;
  intoAlias: string;
  intoTable: string;
  writeMode: "add" | "merge";
  mergeOn: MergeOnRow[];
  /** 寫入欄位（add 模式 = ADD；merge 模式 = NOT MATCHED → ADD） */
  assigns: AssignRow[];
  /** merge 命中時動作 */
  matchedAction: "skip" | "update" | "delete";
  /** matchedAction=update 的 SET 欄位 */
  matchedAssigns: AssignRow[];
  /** merge 未命中時動作 */
  notMatchedAction: "add" | "skip";
}

@Component({
  selector: "app-works",
  imports: [FormsModule, NgTemplateOutlet],
  templateUrl: "./works.page.html",
})
export class WorksPage implements AfterViewInit, OnDestroy {
  private readonly tauri = inject(TauriService);
  private readonly log = inject(LogService);
  readonly ws = inject(WorkspaceService);
  readonly state = inject(EtlStateService);
  readonly output = inject(OutputService);

  private readonly editorHost = viewChild.required<ElementRef<HTMLElement>>("editorHost");
  private view: EditorView | null = null;

  readonly textMode = signal(false);
  readonly works = signal<WorkItem[]>([]);
  readonly selected = signal(-1);

  readonly tables = signal<TableInfo[]>([]);
  readonly targetColumns = signal<ColumnInfo[]>([]);
  /** 別名 → 該別名表的欄位名（值來源 / 條件欄位的建議） */
  readonly aliasCols = signal<Record<string, string[]>>({});
  private readonly colCache = new Map<string, ColumnInfo[]>();

  readonly currentFile = signal<string | null>(null);
  private jobId: string | null = null;

  readonly canRun = computed(() => !this.output.running());
  readonly generators = GENERATORS;
  readonly condOps = COND_OPS;

  constructor() {
    effect(() => {
      const connId = this.ws.targetConnId();
      untracked(() => void this.loadTables(connId));
    });
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
          ".cm-content": { cursor: "text" },
          ".cm-scroller": { cursor: "text" },
        }),
      ],
      parent: this.editorHost().nativeElement,
    });
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

  currentText(): string {
    return this.textMode() ? this.editorText() : this.generateScript();
  }

  async toggleTextMode(): Promise<void> {
    this.output.checkMessage.set(null);
    if (this.textMode()) {
      if (await this.parseIntoModel(this.editorText())) {
        this.textMode.set(false);
      } else {
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

  private findConnByName(name: string) {
    const target = name.trim().toLowerCase();
    return this.ws.connections().find((c) => c.name.trim().toLowerCase() === target) ?? null;
  }

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
      this.ws.sourceConnId.set(null);
      this.state.sourceKind.set("file");
      this.state.sourcePath.set(src.path);
      this.state.sheet.set(src.sheet ?? "");
      this.state.encoding.set(src.encoding);
      this.state.hasHeader.set(src.hasHeader ?? true);
    }
    return warnings;
  }

  // ---- 模型 → UI（解析方向） ----

  private modelToWorks(m: ScriptModel): WorkItem[] {
    return m.works.map((w, i) => this.workFromModel(w, i));
  }

  private exprToText(e: ExprModel): string {
    switch (e.kind) {
      case "col":
        return `${e.alias}.[${e.column}]`;
      case "text":
        return `N'${e.value.replace(/'/g, "''")}'`;
      case "int":
      case "float":
        return String(e.value);
      case "bool":
        return e.value ? "TRUE" : "FALSE";
      case "null":
        return "NULL";
      case "gen":
        return `Gen.${e.name}`;
      case "concat":
        return e.expr;
    }
  }

  private assignFromModel(a: { targetColumn: string; value: ExprModel }): AssignRow {
    const v = a.value;
    switch (v.kind) {
      case "col":
        return { targetColumn: a.targetColumn, kind: "field", alias: v.alias, column: v.column, value: "" };
      case "gen":
        return { targetColumn: a.targetColumn, kind: "gen", alias: "", column: "", value: v.name };
      case "concat":
        return { targetColumn: a.targetColumn, kind: "concat", alias: "", column: "", value: v.expr };
      default:
        return { targetColumn: a.targetColumn, kind: "literal", alias: "", column: "", value: this.exprToText(v) };
    }
  }

  /** 取條件裡的等值比較列（JOIN ON / merge ON 一定是扁平 AND 的 ==）。 */
  private cmpRows(cond: ConditionModel): { left: ExprModel; op: string; right: ExprModel }[] {
    return cond.rows.filter((r): r is Extract<CondRowModel, { kind: "cmp" }> => r.kind === "cmp");
  }

  private joinOnFromModel(j: JoinModel): JoinOnRow[] {
    const ja = j.binding.alias.toLowerCase();
    return this.cmpRows(j.on).map((c): JoinOnRow => {
      const leftSelf = c.left.kind === "col" && c.left.alias.toLowerCase() === ja;
      const self = leftSelf ? c.left : c.right;
      const other = leftSelf ? c.right : c.left;
      return {
        leftAlias: other.kind === "col" ? other.alias : "",
        leftColumn: other.kind === "col" ? other.column : "",
        rightColumn: self.kind === "col" ? self.column : "",
      };
    });
  }

  private blankFilter(): FilterRow {
    return { alias: "", column: "", op: "==", value: "", values: "", low: "", high: "" };
  }

  private filterFromRow(r: CondRowModel): FilterRow {
    const f = this.blankFilter();
    const setField = (e: ExprModel) => {
      if (e.kind === "col") {
        f.alias = e.alias;
        f.column = e.column;
      }
    };
    switch (r.kind) {
      case "cmp":
        if (r.left.kind === "col") {
          setField(r.left);
          f.op = r.op;
          f.value = this.exprToText(r.right);
        } else {
          setField(r.right);
          f.op = r.op;
          f.value = this.exprToText(r.left);
        }
        break;
      case "empty":
        setField(r.expr);
        f.op = r.negated ? "IS NOT EMPTY" : "IS EMPTY";
        break;
      case "in":
        setField(r.expr);
        f.op = r.negated ? "NOT IN" : "IN";
        f.values = r.list.map((e) => this.exprToText(e)).join(", ");
        break;
      case "like":
        setField(r.expr);
        f.op = r.negated ? "NOT LIKE" : "LIKE";
        f.value = this.exprToText(r.pattern);
        break;
      case "between":
        setField(r.expr);
        f.op = r.negated ? "NOT BETWEEN" : "BETWEEN";
        f.low = this.exprToText(r.low);
        f.high = this.exprToText(r.high);
        break;
    }
    return f;
  }

  private mergeOnFromModel(cond: ConditionModel, intoAlias: string): MergeOnRow[] {
    const ia = intoAlias.toLowerCase();
    return this.cmpRows(cond).map((c): MergeOnRow => {
      const leftTarget = c.left.kind === "col" && c.left.alias.toLowerCase() === ia;
      const targetCol = leftTarget
        ? c.left.kind === "col"
          ? c.left.column
          : ""
        : c.right.kind === "col"
          ? c.right.column
          : "";
      const rhs = leftTarget ? c.right : c.left;
      if (rhs.kind === "col") {
        return { targetColumn: targetCol, op: c.op, rhsKind: "field", rhsAlias: rhs.alias, rhsColumn: rhs.column, rhsLiteral: "" };
      }
      return { targetColumn: targetCol, op: c.op, rhsKind: "literal", rhsAlias: "", rhsColumn: "", rhsLiteral: this.exprToText(rhs) };
    });
  }

  private workFromModel(w: ScriptWorkModel, index: number): WorkItem {
    const intoAlias = w.into.alias ?? "";
    const matched: ActionModel | null = w.merge ? w.merge.matched : null;
    const notMatched: ActionModel | null = w.merge ? w.merge.notMatched : null;
    const addAction: ActionModel | null = w.merge ? notMatched : w.action;
    const whereSimple = w.where ? w.where.simple : true;
    return {
      name: w.name ?? `作業 ${index + 1}`,
      fromAlias: w.from.alias,
      fromConn: w.from.conn,
      fromTable: w.from.table.join("."),
      joins: w.joins.map((j) => ({
        alias: j.binding.alias,
        conn: j.binding.conn,
        table: j.binding.table.join("."),
        policy: j.policy,
        on: this.joinOnFromModel(j),
      })),
      whereSimple,
      filters: w.where && whereSimple ? w.where.rows.map((r) => this.filterFromRow(r)) : [],
      whereRaw: w.where ? w.where.raw : "",
      intoAlias,
      intoTable: w.into.table.join("."),
      writeMode: w.merge ? "merge" : "add",
      mergeOn: w.merge ? this.mergeOnFromModel(w.merge.on, intoAlias) : [],
      assigns: (addAction?.assignments ?? []).map((a) => this.assignFromModel(a)),
      matchedAction: matched?.kind === "update" ? "update" : matched?.kind === "delete" ? "delete" : "skip",
      matchedAssigns:
        matched?.kind === "update" ? matched.assignments.map((a) => this.assignFromModel(a)) : [],
      notMatchedAction: notMatched?.kind === "add" ? "add" : w.merge ? "skip" : "add",
    };
  }

  // ---- UI → DSL（產生方向） ----

  private esc(s: string): string {
    return s.replace(/'/g, "''");
  }

  private connKw(conn: ConnKind): string {
    return conn === "source" ? "SOURCE" : "TARGET";
  }

  private tableRef(table: string): string {
    return table
      .split(".")
      .filter((p) => p)
      .map((p) => `[${p}]`)
      .join(".");
  }

  private bindingDsl(conn: ConnKind, table: string): string {
    const ref = this.tableRef(table);
    return ref ? `${this.connKw(conn)}.${ref}` : this.connKw(conn);
  }

  /** 指派常值正規化：數字 / TRUE / FALSE / NULL / 已加引號者原樣，其餘包成 N'…'。 */
  private literalText(raw: string): string {
    const t2 = raw.trim();
    if (!t2) {
      return "NULL";
    }
    if (/^-?\d+(\.\d+)?$/.test(t2) || /^(TRUE|FALSE|NULL)$/i.test(t2) || /^N?'[\s\S]*'$/i.test(t2)) {
      return t2;
    }
    return `N'${this.esc(t2)}'`;
  }

  /** 條件 RHS：除常值外，也讓欄位參照（含 `.[`）原樣通過。 */
  private condValue(raw: string): string {
    const t2 = raw.trim();
    if (!t2) {
      return "NULL";
    }
    if (/\.\[/.test(t2) || /^[A-Za-z_]\w*\.[A-Za-z_]/.test(t2)) {
      return t2;
    }
    return this.literalText(t2);
  }

  private assignDsl(w: WorkItem, a: AssignRow): string {
    switch (a.kind) {
      case "gen":
        return `Gen.${a.value || "GUID"}`;
      case "field":
        return `${a.alias || w.fromAlias}.[${a.column}]`;
      case "literal":
        return this.literalText(a.value);
      case "concat":
        return a.value.trim() || "NULL";
    }
  }

  private filterDsl(f: FilterRow): string {
    const field = `${f.alias}.[${f.column}]`;
    switch (f.op) {
      case "IS EMPTY":
      case "IS NOT EMPTY":
        return `${field} ${f.op}`;
      case "IN":
      case "NOT IN": {
        const list = f.values
          .split(",")
          .map((s) => s.trim())
          .filter((s) => s)
          .map((s) => this.condValue(s))
          .join(", ");
        return `${field} ${f.op} (${list})`;
      }
      case "BETWEEN":
      case "NOT BETWEEN":
        return `${field} ${f.op} ${this.condValue(f.low)} AND ${this.condValue(f.high)}`;
      default: // == != > < >= <= LIKE NOT LIKE
        return `${field} ${f.op} ${this.condValue(f.value)}`;
    }
  }

  /** 篩選列是否完整可輸出（IS EMPTY 類僅需欄位）。 */
  private filterReady(f: FilterRow): boolean {
    if (!f.alias || !f.column) {
      return false;
    }
    if (f.op === "IS EMPTY" || f.op === "IS NOT EMPTY") {
      return true;
    }
    if (f.op === "IN" || f.op === "NOT IN") {
      return f.values.trim().length > 0;
    }
    if (f.op === "BETWEEN" || f.op === "NOT BETWEEN") {
      return f.low.trim().length > 0 && f.high.trim().length > 0;
    }
    return f.value.trim().length > 0;
  }

  private mergeRowDsl(w: WorkItem, r: MergeOnRow): string {
    const rhs =
      r.rhsKind === "field" ? `${r.rhsAlias}.[${r.rhsColumn}]` : this.condValue(r.rhsLiteral);
    return `${w.intoAlias}.[${r.targetColumn}] ${r.op} ${rhs}`;
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
      lines.push(`  FROM ${w.fromAlias} = ${this.bindingDsl(w.fromConn, w.fromTable)}`);

      for (const j of w.joins) {
        const on = j.on
          .filter((r) => r.leftAlias && r.leftColumn && r.rightColumn)
          .map((r) => `${r.leftAlias}.[${r.leftColumn}] == ${j.alias}.[${r.rightColumn}]`)
          .join(" && ");
        const policy = j.policy === "left" ? "LEFT " : "";
        lines.push(`  ${policy}JOIN ${j.alias} = ${this.bindingDsl(j.conn, j.table)} ON ( ${on} )`);
      }

      // WHERE：simple → 由 filters 產生；進階（含 OR/NOT）→ 原樣輸出 raw
      if (!w.whereSimple) {
        if (w.whereRaw.trim()) {
          lines.push(`  WHERE ${w.whereRaw.trim()}`);
        }
      } else {
        const filters = w.filters.filter((f) => this.filterReady(f));
        if (filters.length) {
          lines.push(`  WHERE ${filters.map((f) => this.filterDsl(f)).join(" && ")}`);
        }
      }

      const intoPrefix = w.intoAlias ? `${w.intoAlias} = ` : "";
      lines.push(`  INTO ${intoPrefix}TARGET.${this.tableRef(w.intoTable)}`);

      if (w.writeMode === "merge") {
        const on = w.mergeOn
          .filter((r) => r.targetColumn)
          .map((r) => this.mergeRowDsl(w, r))
          .join(" &&\n       ");
        lines.push(`  ON ( ${on} )`);
        if (w.notMatchedAction === "add") {
          lines.push("  NOT MATCHED {");
          this.pushAssignBlock(lines, w, w.assigns, "ADD", "    ");
          lines.push("  }");
        }
        if (w.matchedAction === "update") {
          lines.push("  MATCHED {");
          this.pushAssignBlock(lines, w, w.matchedAssigns, "UPDATE", "    ");
          lines.push("  }");
        } else if (w.matchedAction === "delete") {
          lines.push("  MATCHED { DELETE }");
        }
      } else {
        this.pushAssignBlock(lines, w, w.assigns, "ADD", "  ");
      }
      lines.push("}");
    }
    lines.push("GO");
    lines.push("");
    return lines.join("\n");
  }

  private pushAssignBlock(
    lines: string[],
    w: WorkItem,
    assigns: AssignRow[],
    verb: "ADD" | "UPDATE",
    indent: string,
  ): void {
    lines.push(`${indent}${verb} {`);
    assigns
      .filter((a) => a.targetColumn)
      .forEach((a, i) => {
        lines.push(`${indent}  ${i === 0 ? " " : ","}[${a.targetColumn}] = ${this.assignDsl(w, a)}`);
      });
    lines.push(`${indent}}`);
  }

  private effectiveSourceLine(): string | null {
    const conn = this.ws.sourceConnection();
    if (conn) {
      if (conn.kind === "file") {
        return `SOURCE = CONNECTION('${this.esc(conn.name)}')`;
      }
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

  private effectiveTargetLine(): string | null {
    const conn = this.ws.targetConnection();
    return conn ? `TARGET = CONNECTION('${this.esc(conn.name)}')` : null;
  }

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
      const t2 = lines[rest].trim();
      if (/^SOURCE\s*=/i.test(t2)) {
        oldSrc = lines[rest];
        continue;
      }
      if (/^TARGET\s*=/i.test(t2)) {
        oldTgt = lines[rest];
        continue;
      }
      if (t2 === "" || t2.startsWith("--") || t2.startsWith("//")) {
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
      fromAlias: "src",
      fromConn: "source",
      fromTable: "",
      joins: [],
      whereSimple: true,
      filters: [],
      whereRaw: "",
      intoAlias: "",
      intoTable: "",
      writeMode: "add",
      mergeOn: [],
      assigns: [{ targetColumn: "", kind: "field", alias: "src", column: "", value: "" }],
      matchedAction: "skip",
      matchedAssigns: [],
      notMatchedAction: "add",
    };
    this.works.update((a) => [...a, item]);
    this.selectWork(this.works().length - 1);
  }

  removeWork(i: number): void {
    this.works.update((a) => a.filter((_, idx) => idx !== i));
    const n = this.works().length;
    this.selected.set(Math.min(this.selected(), n - 1));
  }

  worksChanged(): void {
    this.works.update((a) => [...a]);
  }

  /** 主來源 / 別名 / 表變更後重載欄位建議。 */
  worksChangedReload(): void {
    this.worksChanged();
    void this.refreshColumnsForSelected();
  }

  // 關聯表（JOIN）
  addJoin(w: WorkItem): void {
    const alias = this.uniqueAlias(w, "lookup");
    w.joins.push({
      alias,
      conn: "target",
      table: "",
      policy: "inner",
      on: [{ leftAlias: w.fromAlias, leftColumn: "", rightColumn: "" }],
    });
    this.worksChangedReload();
  }

  removeJoin(w: WorkItem, i: number): void {
    w.joins.splice(i, 1);
    this.worksChangedReload();
  }

  addJoinOn(j: JoinItem, w: WorkItem): void {
    j.on.push({ leftAlias: w.fromAlias, leftColumn: "", rightColumn: "" });
    this.worksChanged();
  }

  removeJoinOn(j: JoinItem, i: number): void {
    j.on.splice(i, 1);
    this.worksChanged();
  }

  // 篩選（WHERE）
  addFilter(w: WorkItem): void {
    w.filters.push({ ...this.blankFilter(), alias: w.fromAlias, op: "IS NOT EMPTY" });
    this.worksChanged();
  }

  removeFilter(w: WorkItem, i: number): void {
    w.filters.splice(i, 1);
    this.worksChanged();
  }

  isEmptyOp(op: string): boolean {
    return op === "IS EMPTY" || op === "IS NOT EMPTY";
  }

  isInOp(op: string): boolean {
    return op === "IN" || op === "NOT IN";
  }

  isBetweenOp(op: string): boolean {
    return op === "BETWEEN" || op === "NOT BETWEEN";
  }

  /** 單一值輸入（比較 / LIKE）；IS EMPTY 無值、IN 為清單、BETWEEN 為上下界。 */
  opNeedsValue(op: string): boolean {
    return !this.isEmptyOp(op) && !this.isInOp(op) && !this.isBetweenOp(op);
  }

  /** 進階條件（含 OR/NOT）→ 清空 raw，回到可視化 simple 編輯。 */
  clearAdvancedWhere(w: WorkItem): void {
    w.whereSimple = true;
    w.whereRaw = "";
    w.filters = [];
    this.worksChanged();
  }

  // 寫入模式 / merge
  onWriteModeChange(w: WorkItem, mode: "add" | "merge"): void {
    w.writeMode = mode;
    if (mode === "merge") {
      if (!w.intoAlias) {
        w.intoAlias = this.deriveAlias(w.intoTable, "mapping");
      }
      if (!w.mergeOn.length) {
        w.mergeOn.push({
          targetColumn: "",
          op: "==",
          rhsKind: "field",
          rhsAlias: w.fromAlias,
          rhsColumn: "",
          rhsLiteral: "",
        });
      }
    }
    this.worksChangedReload();
  }

  addMergeOn(w: WorkItem): void {
    w.mergeOn.push({
      targetColumn: "",
      op: "==",
      rhsKind: "field",
      rhsAlias: w.fromAlias,
      rhsColumn: "",
      rhsLiteral: "",
    });
    this.worksChanged();
  }

  removeMergeOn(w: WorkItem, i: number): void {
    w.mergeOn.splice(i, 1);
    this.worksChanged();
  }

  // 欄位指派（ADD / UPDATE 共用同一列模型）
  addRow(w: WorkItem, list: AssignRow[]): void {
    list.push({ targetColumn: "", kind: "field", alias: w.fromAlias, column: "", value: "" });
    this.worksChanged();
  }

  removeRow(list: AssignRow[], i: number): void {
    list.splice(i, 1);
    this.worksChanged();
  }

  onAssignKindChange(w: WorkItem, a: AssignRow, kind: AssignRow["kind"]): void {
    a.kind = kind;
    if (kind === "gen" && !GENERATORS.some((g) => g.value === a.value)) {
      a.value = "GUID";
    }
    if (kind === "field" && !a.alias) {
      a.alias = w.fromAlias;
    }
    if (kind !== "gen" && GENERATORS.some((g) => g.value === a.value)) {
      a.value = "";
    }
    this.worksChanged();
  }

  // merge 命中時動作（MATCHED）
  onMatchedActionChange(w: WorkItem, action: "skip" | "update" | "delete"): void {
    w.matchedAction = action;
    if (action === "update" && !w.matchedAssigns.length) {
      this.addRow(w, w.matchedAssigns);
    }
    this.worksChanged();
  }

  // ---- 別名 / 欄位中繼資料 ----

  /** 可作為值來源的別名（主來源 + 各關聯表）。 */
  sourceAliases(w: WorkItem): string[] {
    return [w.fromAlias, ...w.joins.map((j) => j.alias)].filter((a) => a);
  }

  /** 需要欄位建議的所有別名（含 INTO 別名供 merge 目標欄位）。 */
  private allAliases(w: WorkItem): string[] {
    const set = new Set<string>(this.sourceAliases(w));
    if (w.intoAlias) {
      set.add(w.intoAlias);
    }
    return [...set];
  }

  /** 渲染欄位建議 datalist 用的別名清單（樣板呼叫）。 */
  datalistAliases(w: WorkItem): string[] {
    return this.allAliases(w);
  }

  aliasListId(alias: string): string {
    return "wal-" + alias;
  }

  columnsForAlias(alias: string): string[] {
    return this.aliasCols()[alias] ?? [];
  }

  private aliasTable(w: WorkItem, alias: string): { conn: ConnKind; table: string } | null {
    if (alias === w.fromAlias) {
      return { conn: w.fromConn, table: w.fromTable };
    }
    const j = w.joins.find((x) => x.alias === alias);
    if (j) {
      return { conn: j.conn, table: j.table };
    }
    if (alias === w.intoAlias) {
      return { conn: "target", table: w.intoTable };
    }
    return null;
  }

  private uniqueAlias(w: WorkItem, base: string): string {
    const taken = this.allAliases(w).map((a) => a.toLowerCase());
    if (!taken.includes(base)) {
      return base;
    }
    for (let n = 2; ; n++) {
      const cand = `${base}${n}`;
      if (!taken.includes(cand)) {
        return cand;
      }
    }
  }

  private deriveAlias(table: string, fallback: string): string {
    const last = table.split(".").filter((p) => p).pop() ?? "";
    const base = last.replace(/[^A-Za-z0-9_]/g, "").toLowerCase() || fallback;
    return base;
  }

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

  tableKey(t2: TableInfo): string {
    return t2.schema ? `${t2.schema}.${t2.name}` : t2.name;
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

  /** 重新整理目標欄位（INTO）+ 各別名的欄位建議。 */
  private async refreshColumnsForSelected(): Promise<void> {
    const w = this.selectedWork();
    if (!w) {
      this.targetColumns.set([]);
      this.aliasCols.set({});
      return;
    }
    this.targetColumns.set(await this.loadColumns(w.intoTable));
    const map: Record<string, string[]> = {};
    for (const alias of this.allAliases(w)) {
      const at = this.aliasTable(w, alias);
      if (!at) {
        continue;
      }
      if (at.conn === "source" && alias === w.fromAlias) {
        map[alias] = this.sourceColumnNames();
      } else {
        map[alias] = (await this.loadColumns(at.table)).map((c) => c.name);
      }
    }
    this.aliasCols.set(map);
  }

  sourceColumnNames(): string[] {
    return this.state.preview()?.columns.map((c) => c.name) ?? [];
  }

  /** 以欄名（不分大小寫）自動對應來源欄位 → 目標表欄位。 */
  async autoMatch(): Promise<void> {
    const w = this.selectedWork();
    const preview = this.state.preview();
    if (!w || !preview || !w.intoTable) {
      return;
    }
    await this.refreshColumnsForSelected();
    const byName = new Map(this.targetColumns().map((c) => [c.name.toLowerCase(), c]));
    const rows: AssignRow[] = [];
    for (const c of preview.columns) {
      const hit = byName.get(c.name.toLowerCase());
      if (hit) {
        rows.push({ targetColumn: hit.name, kind: "field", alias: w.fromAlias, column: c.name, value: "" });
      }
    }
    if (rows.length) {
      w.assigns = rows;
      this.worksChanged();
      this.log.info("作業", `${w.intoTable}：自動對應 ${rows.length} 欄`);
    } else {
      this.log.warn("作業", "沒有名稱相符的欄位（請先於「匯入資料」載入來源）");
    }
  }

  // ---- 檔案 / 驗證 / 執行 ----

  etlFileName(): string {
    const p = this.currentFile();
    return p ? (p.split(/[/\\]/).pop() ?? p) : "未命名";
  }

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
