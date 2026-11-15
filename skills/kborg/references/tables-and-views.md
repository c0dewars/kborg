# Tables, columns, sorting and filters

## views.yaml: columns and sort per resource

This is the k9s format, so an existing k9s `views.yaml` works. `kborg views init` writes the built-in columns as a file to edit (it never replaces one), and `:columns` in the TUI shows the current table's columns, every predefined column and how to change them.

<!-- FILE: views.yaml -->
```yaml
views:
  v1/pods:
    sortColumn: RSTS:desc
    columns:
      - NAMESPACE
      - NAME
      - READY
      - STATUS
      - RSTS
      - IMAGE:.spec.containers[0].image
      - TEAM:.metadata.labels['team']|W
      - AGE
  apps/v1/deployments:
    columns: [NAME, READY, IMAGES, AGE]
```

- **Resource keys:** `v1/pods`, `apps/v1/deployments`, `deployments.apps` or `deployments`. Custom resources work too.
- **Predefined columns** (used by name alone):
  - Any table: `NAMESPACE NAME READY STATUS RSTS AGE NODE IP CPU MEM LABELS IMAGES`
  - Nodes: `ROLE ARCH TAINTS VERSION OS-IMAGE KERNEL RUNTIME INTERNAL-IP EXTERNAL-IP PODS-MAX %CPU %MEM CPU/A MEM/A`
  - Pods: `%CPU/R %CPU/L %MEM/R %MEM/L` (usage against requests or limits), `QOS SA HOST-IP`
  - Workloads: `UP-TO-DATE AVAILABLE DESIRED CURRENT COMPLETIONS SUCCEEDED SCHEDULE SUSPEND LAST-SCHEDULE`
  - Other: `TYPE CLUSTER-IP PORTS CAPACITY VOLUME STORAGECLASS DATA CLASS HOSTS`
- **Custom columns:** `TITLE:.json.path` takes dotted fields, `[N]`, `[*]` for every element, and `['key.with.dots']` for labels and annotations. Paths are read, never evaluated.
- **Attributes** after `|`: `W` shows the column only in wide mode (Ctrl-W), `H` hides it, `N` sorts it as a number, `T` shows a timestamp as an age.
- **`sortColumn`:** a column and `asc` or `desc`, applied when the resource opens.
- **Narrow terminals:** columns are dropped from the right, and NAME always stays.
- **Limits:** 128 views, 24 columns per view, 64 KiB.
- **Speed:** for ConfigMaps, ServiceAccounts, RBAC objects and similar kinds, KBorg normally downloads only metadata. A custom column that reads more than name, namespace, labels and timestamps makes that table download full objects, which is a little slower on big clusters.

Recipes:
- **Image and node on every Pod:** add `IMAGES` and `NODE` to `v1/pods`.
- **Owner team from a label:** `TEAM:.metadata.labels['team']`.
- **Requests vs usage:** `%CPU/R` and `%MEM/R` on Pods (`n/a` when a container sets no request).

## Sorting

- **Keys:** `N` `A` `S` `R` `T` `P` `C` `M` sort by name, age, status, readiness, restarts, namespace, CPU and memory. `O` and `I` sort Pods by node and IP. The same key again reverses the order.
- **Moving the sort:** `Shift-Right` and `Shift-Left` move the sort to the next column, and `Shift-Down` and `Shift-Up` set descending and ascending.
- **Other ways:** a click on a column title sorts by it, and `:sort COLUMN [asc|desc]` sorts by any column, custom ones included.
- **Marker:** the sorted column shows `^` or `v`.

Check `kborg keymap --json` for the person's actual sort keys.

## Filtering and scope

| Type | Does |
|---|---|
| `/TEXT` | Regex over namespace, name and status |
| `/!TEXT` | Inverse |
| `/-f TEXT` | Fuzzy |
| `/-l app=web` | Server-side label selector |
| `:pod app=web,tier!=batch` or `:pod 'app in (api,web)'` | Selector in the command |
| `0` / `1`..`9` | All namespaces / the nine most recently used namespaces |
| Ctrl-Z or `:faults` | Only objects that need attention |
| Ctrl-W | Wide columns |
| Space | Mark rows (for a batch delete, restart, suspend or resume) |

## Saving a view

`b` in a resource table saves it under a name: context, namespace, resource and label selector, plus the selected workload. `B` reopens saved views (and saved dashboards). Text filters are not saved; a label selector is. Limits: 16 selections, names of 1 to 64 plain characters.
