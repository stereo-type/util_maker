---
name: next-task
description: Pick and implement one available Util Maker backlog task, across the project or within a named component. Use for requests such as “бери следующую задачу” or “take the next task in plugins/screenshot”.
---

# Take the next Util Maker task

1. Open the [backlog index](../../../docs/tasks/README.md). For a named component, follow its exact queue link. A plugin and its providers have separate queues. Without a scope, check general tasks first, then component queues in the listed order. Within a queue choose the first card marked **📥 К выполнению** whose start dependencies are satisfied. A dependency needed only for later Windows acceptance allows Linux work to start. If the requested scope has no eligible card, report that and finish.
2. Read the card and its linked specification. Recheck availability before claiming it. Mark the card and its row **🛠️ В работе** together. The owning row is in the component queue for a component task or in the general tasks section of the backlog index for a general task. If another agent has claimed it, repeat selection within the requested scope. Use distinct scopes or assigned cards for concurrent work in separate checkouts, as described in the [task guide](../../../docs/task-workflow.md).
3. Implement the acceptance criteria using the [project instructions](../../../AGENTS.md) for Make targets, Linux checks and frontend builds. Record commands and observed outcomes under **Проверка** in the card. Tick only verified criteria. If progress is blocked, set **⛔ Заблокирована** in both places and record the condition for resuming.
4. Set the card and its row to **👁️ На ревью** when ready. For outstanding Windows acceptance, give the user the revision, local build command, manual steps and expected result. Return the card link and verification summary. This invocation covers one card. Reserve **✅ Выполнена** for completed review and confirmed acceptance criteria.
