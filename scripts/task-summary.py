import re
from collections import Counter
from pathlib import Path


TASKS = Path(__file__).resolve().parents[1] / "docs" / "tasks"
GROUPS = {
    "К выполнению": "Не сделано",
    "В работе": "Не сделано",
    "Заблокирована": "Не сделано",
    "На ревью": "На ревью",
    "Выполнена": None,
}
QUEUE_LINK = re.compile(r"\[([^\]]+)\]\((blocks/[^)]+/README\.md)\)")


def table_rows(path, first_column):
    in_table = False
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.startswith("|"):
            in_table = False
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        if cells[0] == first_column:
            in_table = True
        elif in_table and not cells[0].startswith("---"):
            yield cells


def main():
    index = TASKS / "README.md"
    queues = [("Общие задачи", "README.md#общие-задачи", index)]
    for _, queue_cell, *_ in table_rows(index, "Блок"):
        link = QUEUE_LINK.fullmatch(queue_cell)
        if not link:
            raise ValueError(f"{index}: не удалось прочитать ссылку на очередь {queue_cell}")
        name, target = link.groups()
        queues.append((name, target, TASKS / target))

    lines = [
        "# Незавершённые задачи по разделам",
        "",
        "«Не сделано» объединяет статусы «К выполнению», «В работе» и «Заблокирована». "
        "Задачи на ревью учитываются отдельно.",
        "",
        "| Раздел | Состояние | Количество |",
        "| --- | --- | ---: |",
    ]
    for name, target, path in queues:
        counts = Counter()
        for _, status, *_ in table_rows(path, "Задача"):
            label = re.sub(r"^\W+", "", status)
            if label not in GROUPS:
                raise ValueError(f"{path}: неизвестный статус {status}")
            group = GROUPS[label]
            if group:
                counts[group] += 1
        for group in ("Не сделано", "На ревью"):
            if counts[group] > 0:
                lines.append(f"| [{name}]({target}) | {group} | {counts[group]} |")

    (TASKS / "open-tasks.md").write_text("\n".join(lines) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
