import { api } from "../api";
import { PageContent } from "../components/layout/PageContent";
import { Button } from "../components/ui/Button";
import { TitledCard } from "../components/ui/TitledCard";
import { useMessage } from "../components/ui/message";
import styles from "./CompactionConfigPage.module.scss";

const compactionPromptPath = "~/.cursor-byok-v3/rules/compaction.md";

export function CompactionConfigPage() {
  const message = useMessage();
  const openPrompt = async () => {
    try {
      await api.openCompactionPrompt();
    } catch (cause) {
      message(cause instanceof Error ? cause.message : String(cause));
    }
  };

  return <PageContent title={t("配置")} sections={[{
    key: "compaction-config",
    estimatedHeight: 240,
    content: <div className={styles.page}>
      <TitledCard title={t("对话压缩提示词")} action={<Button variant="primary" size="small" onClick={() => void openPrompt()}>{t("打开配置文件")}</Button>}>
        <div className={styles.content}>
          <p>{t("当对话接近模型上下文上限时，Cursor BYOK 使用此提示词生成会话摘要。保存文件后的下一次压缩会自动使用新内容。")}</p>
          <code>{compactionPromptPath}</code>
        </div>
      </TitledCard>
    </div>,
  }]} />;
}
