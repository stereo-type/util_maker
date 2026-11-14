---
name: next-task
description: Use when the user asks to pick up and implement the next available task from the Util Maker backlog, including “бери следующую задачу” or “take the next task”.
---

# Take the next Util Maker task

1. Read the [task index](../../../docs/tasks/README.md) from top to bottom. Choose the first card marked **К выполнению** whose dependencies allow work to begin. A dependency needed only for a later Windows check does not prevent starting work on Linux. If no card qualifies, report why and stop.
2. Read the card and its linked specification. Before implementation, set the card and its index row to **В работе** in the same change. Keep both in sync at every transition.
3. Implement the card against its acceptance criteria. Follow the [project instructions](../../../AGENTS.md) for Make targets, Linux checks, and frontend builds. Record commands and observed results under **Проверка** in the card. Tick only verified criteria. If a blocker prevents further progress, set **Заблокирована** and record what would unblock it.
4. When the change is ready for review, set **На ревью**. For outstanding Windows checks, leave the criteria open and give the user the revision, local build command, steps, and expected result. Finish with the card link and verification summary. This invocation covers one card. Reserve **Выполнена** for completed review and confirmed acceptance criteria.
