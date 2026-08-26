import { useEffect, useRef } from "react";
import Sortable from "sortablejs";
import type { Model } from "../../api";
import { Button } from "../ui/Button";
import { Card } from "../ui/Card";
import { Icon } from "../ui/Icon";
import { claudeIcon, dragIcon, openAiIcon } from "../ui/icons";
import { CursorModelTestResult, type CursorModelTestState } from "./CursorModelTestResult";
import styles from "./CursorSettings.module.scss";

export function CursorModelCards({
  models,
  disabled,
  testingModelHashes,
  testResults,
  onTest,
  onEdit,
  onDuplicate,
  onDelete,
  onReorder,
}: {
  models: Model[];
  disabled: boolean;
  testingModelHashes: Set<string>;
  testResults: Map<string, CursorModelTestState>;
  onTest: (model: Model) => void;
  onEdit: (model: Model) => void;
  onDuplicate: (model: Model) => void;
  onDelete: (model: Model) => void;
  onReorder: (modelHashes: string[]) => void;
}) {
  const grid = useRef<HTMLDivElement>(null);
  const sortable = useRef<Sortable | null>(null);
  const currentModels = useRef(models);
  const reorder = useRef(onReorder);
  currentModels.current = models;
  reorder.current = onReorder;

  useEffect(() => {
    if (!grid.current) return;
    sortable.current = Sortable.create(grid.current, {
      animation: 160,
      dataIdAttr: "data-model-hash",
      draggable: `.${styles.modelCard}`,
      handle: `.${styles.sortHandle}`,
      ghostClass: styles.sortGhost,
      chosenClass: styles.sortChosen,
      dragClass: styles.sortDragging,
      forceFallback: true,
      fallbackOnBody: true,
      fallbackTolerance: 3,
      onEnd: (event) => {
        const oldIndex = event.oldDraggableIndex ?? event.oldIndex;
        const newIndex = event.newDraggableIndex ?? event.newIndex;
        if (typeof oldIndex !== "number"
          || typeof newIndex !== "number"
          || oldIndex === newIndex) {
          sortable.current?.sort(currentModels.current.map((model) => model.model_hash), false);
          return;
        }
        const reordered = currentModels.current.slice();
        const [moved] = reordered.splice(oldIndex, 1);
        if (!moved || newIndex < 0 || newIndex > reordered.length) {
          sortable.current?.sort(currentModels.current.map((model) => model.model_hash), false);
          return;
        }
        reordered.splice(newIndex, 0, moved);
        reorder.current(reordered.map((model) => model.model_hash));
      },
    });
    return () => {
      sortable.current?.destroy();
      sortable.current = null;
    };
  }, []);

  useEffect(() => {
    sortable.current?.option("disabled", disabled);
    sortable.current?.sort(models.map((model) => model.model_hash), false);
  }, [disabled, models]);

  return <div ref={grid} className={styles.modelGrid}>
    {models.map((model) => {
      const result = testResults.get(model.model_hash);
      const testing = testingModelHashes.has(model.model_hash);
      return <Card className={styles.modelCard} data-model-hash={model.model_hash} key={model.model_hash}>
        <button type="button" className={styles.sortHandle} disabled={disabled} aria-label={t("拖动排序")} title={t("拖动排序")} onClick={(event) => event.stopPropagation()}>
          <Icon icon={dragIcon} size="1.25em" />
        </button>
        <div className={styles.modelCardContent}>
          <div className={styles.modelCardTop}>
            <div className={styles.modelCardName}>
              <strong>{model.display_name}</strong>
              <span>{model.model_id}</span>
            </div>
            <span className={styles.modelTypeBadge}>
              <Icon icon={model.type === "anthropic" ? claudeIcon : openAiIcon} />
              {model.type === "anthropic" ? "Anthropic" : "OpenAI"}
            </span>
          </div>
          <div className={styles.modelCardTest}>
            <CursorModelTestResult state={result} testing={testing} />
          </div>
          <div className={styles.modelCardActions}>
            <Button size="small" disabled={disabled} onClick={() => onTest(model)}>{testing ? t("测试中…") : t("测试")}</Button>
            <Button size="small" disabled={disabled} onClick={() => onEdit(model)}>{t("编辑")}</Button>
            <Button size="small" disabled={disabled} onClick={() => onDuplicate(model)}>{t("复制")}</Button>
            <Button size="small" className={styles.deleteButton} disabled={disabled} onClick={() => onDelete(model)}>{t("删除")}</Button>
          </div>
        </div>
      </Card>;
    })}
  </div>;
}
