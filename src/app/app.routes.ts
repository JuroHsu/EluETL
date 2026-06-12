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
    path: "mapping",
    loadComponent: () =>
      import("./pages/mapping/mapping.page").then((m) => m.MappingPage),
  },
  {
    path: "execute",
    loadComponent: () =>
      import("./pages/execute/execute.page").then((m) => m.ExecutePage),
  },
];
