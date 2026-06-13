import { Routes } from "@angular/router";

export const routes: Routes = [
  { path: "", pathMatch: "full", redirectTo: "connections" },
  {
    path: "connections",
    loadComponent: () =>
      import("./pages/connections/connections.page").then((m) => m.ConnectionsPage),
  },
  {
    path: "import",
    loadComponent: () =>
      import("./pages/import/import.page").then((m) => m.ImportPage),
  },
  {
    path: "works",
    loadComponent: () =>
      import("./pages/works/works.page").then((m) => m.WorksPage),
  },
  // 舊路由：欄位對應與 ETL 腳本已整合為「遷移作業」
  { path: "mapping", redirectTo: "works" },
  { path: "script", redirectTo: "works" },
  { path: "execute", redirectTo: "works" },
];
