import type { RefObject } from "react";
import { Icon } from "../../shared/ui/Icon";
import { windowCloseIcon } from "../../shared/ui/icons";
import type { AdSlot } from "./types";
import styles from "./Ads.module.scss";

type AdMenuProps = {
  ads: AdSlot[];
  activeAdId?: string;
  dismissingAdId?: string;
  readAdIds: ReadonlySet<string>;
  triggerRefs: RefObject<Map<string, HTMLButtonElement>>;
  onOpen: (ad: AdSlot) => void;
  onDismiss: (ad: AdSlot) => void;
};

export function AdMenu({ ads, activeAdId, dismissingAdId, readAdIds, triggerRefs, onOpen, onDismiss }: AdMenuProps) {
  return <div className={styles.menuSlots} aria-label={t("推荐内容")}>
    {ads.map((ad) => <div key={ad.id} className={styles.menuSlotContainer}>
      <button
        ref={(node) => {
          if (node) triggerRefs.current.set(ad.id, node);
          else triggerRefs.current.delete(ad.id);
        }}
        type="button"
        className={styles.menuSlot}
        title={`${ad.target.title}\n${ad.target.description}`}
        aria-label={`${ad.target.title}，${ad.target.description}${readAdIds.has(ad.id) ? "" : `，${t("未读")}`}`}
        aria-haspopup="dialog"
        aria-expanded={activeAdId === ad.id}
        aria-controls={activeAdId === ad.id ? "menu-ad-dialog" : undefined}
        onClick={() => onOpen(ad)}
      >
        {!readAdIds.has(ad.id) && <span className={styles.unreadDot} aria-hidden="true" />}
        <img className={styles.menuImage} src={ad.target.imageUrl} alt="" />
        <span className={styles.menuCopy}>
          <span className={styles.menuTitle}>{ad.target.title}</span>
          <span className={styles.menuSubtitle}>{ad.target.description}</span>
        </span>
      </button>
      <button
        type="button"
        className={styles.menuDismiss}
        aria-label={`${t("不再显示广告")}：${ad.target.title}`}
        aria-haspopup="dialog"
        aria-expanded={dismissingAdId === ad.id}
        aria-controls={dismissingAdId === ad.id ? "dismiss-ad-dialog" : undefined}
        onClick={() => onDismiss(ad)}
      >
        <Icon icon={windowCloseIcon} size="1em" />
      </button>
    </div>)}
  </div>;
}
