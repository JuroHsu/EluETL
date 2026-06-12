import {
  Component,
  ElementRef,
  OnInit,
  effect,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { FormsModule } from "@angular/forms";
import { RouterLink, RouterLinkActive, RouterOutlet } from "@angular/router";

import { LogService } from "./services/log.service";
import { WorkspaceService } from "./services/workspace.service";

@Component({
  selector: "app-root",
  imports: [RouterOutlet, RouterLink, RouterLinkActive, FormsModule],
  templateUrl: "./app.component.html",
})
export class AppComponent implements OnInit {
  readonly ws = inject(WorkspaceService);
  readonly log = inject(LogService);

  readonly panelOpen = signal(true);
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
}
