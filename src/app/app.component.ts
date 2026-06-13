import {
  Component,
  ElementRef,
  OnInit,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { RouterLink, RouterLinkActive, RouterOutlet } from "@angular/router";
import { openUrl } from "@tauri-apps/plugin-opener";

import { LogService } from "./services/log.service";
import { WorkspaceService } from "./services/workspace.service";

@Component({
  selector: "app-root",
  imports: [RouterOutlet, RouterLink, RouterLinkActive, FormsModule],
  templateUrl: "./app.component.html",
  host: { "(document:keydown.escape)": "infoOpen.set(false)" },
})
export class AppComponent implements OnInit {
  readonly ws = inject(WorkspaceService);
  readonly log = inject(LogService);

  readonly panelOpen = signal(true);
  /** 「資訊」視窗開關 */
  readonly infoOpen = signal(false);
  readonly version = "0.1.0";
  private readonly logBox = viewChild<ElementRef<HTMLElement>>("logBox");

  constructor() {
    // log 新增時自動捲到底
    effect(() => {
      this.log.entries();
      const el = this.logBox()?.nativeElement;
      if (el) {
        queueMicrotask(() => (el.scrollTop = el.scrollHeight));
      }
    });
    // 切換目標連線時 ping，更新狀態列指示燈
    effect(() => {
      this.ws.targetConnId();
      untracked(() => void this.ws.pingActive());
    });
  }

  async ngOnInit(): Promise<void> {
    try {
      await this.ws.reload();
    } catch {
      // 狀態庫尚未就緒時靜默；連線頁會再載入
    }
  }

  timeOf(d: Date): string {
    return d.toLocaleTimeString("zh-TW", { hour12: false });
  }

  /** 以系統預設瀏覽器開啟外部連結（Tauri opener 插件）。 */
  async openExternal(url: string): Promise<void> {
    try {
      await openUrl(url);
    } catch (e) {
      this.log.error("資訊", `無法開啟連結：${String(e)}`);
    }
  }
}
