import { Component } from "@angular/core";

@Component({
  selector: "app-execute",
  template: `
    <div class="mx-auto max-w-2xl">
      <h1 class="mb-6 text-2xl font-semibold text-slate-800">ETL 執行</h1>
      <div class="rounded-lg bg-white p-6 text-sm text-slate-500 shadow">
        即時進度、錯誤列表與續跑（Phase 2 / Week 6 實作）。
      </div>
    </div>
  `,
})
export class ExecutePage {}
