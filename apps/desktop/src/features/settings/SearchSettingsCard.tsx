import type { GrepEngine, SearchSettings } from "../../shared/api";
import { Button } from "../../shared/ui/Button";
import { TextInput } from "../../shared/ui/FormControls";
import { Select } from "../../shared/ui/Select";
import { TitledCard } from "../../shared/ui/TitledCard";
import styles from "./TabSettingsCard.module.scss";

export function SearchSettingsCard({
  settings,
  draft,
  editing,
  saving,
  onDraftChange,
  onEdit,
  onCancel,
  onSave,
}: {
  settings: SearchSettings | null;
  draft: SearchSettings;
  editing: boolean;
  saving: boolean;
  onDraftChange: (settings: SearchSettings) => void;
  onEdit: () => void;
  onCancel: () => void;
  onSave: () => void;
}) {
  const engineLabel = (engine: GrepEngine) => {
    if (engine === "tgrep") return "Microsoft TGrep (Trigram Index ~1ms)";
    if (engine === "auto") return t("自动 (优先使用 TGrep，异常时自动回退)");
    return "Ripgrep (Native rg)";
  };

  const action = editing ? (
    <div className={styles.actionGroup}>
      <Button size="small" disabled={saving} onClick={onCancel}>
        {t("取消")}
      </Button>
      <Button variant="primary" size="small" disabled={saving} onClick={onSave}>
        {saving ? t("保存中…") : t("保存")}
      </Button>
    </div>
  ) : (
    <button type="button" className={styles.headerAction} disabled={!settings} onClick={onEdit}>
      {t("编辑")}
    </button>
  );

  return (
    <TitledCard
      title={
        <div className={styles.title}>
          <span>⚡ {t("代码搜索引擎设置 (Grep / TGrep)")}</span>
        </div>
      }
      action={action}
    >
      <div className={styles.content}>
        {editing ? (
          <>
            <div className={styles.row}>
              <div className={styles.description}>
                <strong>{t("搜索引擎选择")}</strong>
                <small>{t("选择 AI Agent 调用 Grep 工具时使用的后端引擎。")}</small>
              </div>
              <div className={styles.control}>
                <Select
                  value={draft.grep_engine}
                  ariaLabel={t("搜索引擎选择")}
                  options={[
                    { value: "auto", label: t("自动 (优先使用 TGrep，异常时自动回退)") },
                    { value: "tgrep", label: "Microsoft TGrep (Trigram Index ~1ms)" },
                    { value: "ripgrep", label: "Ripgrep (Native rg)" },
                  ]}
                  onChange={(value) =>
                    onDraftChange({ ...draft, grep_engine: value as GrepEngine })
                  }
                />
              </div>
            </div>
            <div className={styles.row}>
              <div className={styles.description}>
                <strong>{t("自定义 tgrep.exe 路径 (可选)")}</strong>
                <small>{t("留空则自动从 PATH 或项目根目录查找。")}</small>
              </div>
              <div className={styles.control}>
                <TextInput
                  value={draft.tgrep_path || ""}
                  placeholder="C:\PROJECTS\cursor-byok\tgrep.exe"
                  onChange={(event) =>
                    onDraftChange({ ...draft, tgrep_path: event.target.value.trim() || null })
                  }
                />
              </div>
            </div>
          </>
        ) : (
          <>
            <div className={styles.row}>
              <strong>{t("当前引擎")}</strong>
              <span>{settings ? engineLabel(settings.grep_engine) : "—"}</span>
            </div>
            <div className={styles.row}>
              <strong>{t("二进制文件路径")}</strong>
              <span>
                {settings?.tgrep_path ? settings.tgrep_path : t("自动识别 (PATH / 项目目录)")}
              </span>
            </div>
          </>
        )}
      </div>
    </TitledCard>
  );
}
