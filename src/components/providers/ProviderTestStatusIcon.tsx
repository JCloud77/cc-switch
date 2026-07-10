import {
  Circle,
  CircleCheck,
  CircleX,
  Loader2,
  type LucideIcon,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { cn } from "@/lib/utils";
import type { ProviderTestStatus } from "@/hooks/useStreamCheck";

interface ProviderTestStatusIconProps {
  providerName: string;
  status?: ProviderTestStatus;
  isTesting: boolean;
}

type DisplayStatus = ProviderTestStatus | "testing" | "untested";

const STATUS_STYLES: Record<
  DisplayStatus,
  { Icon: LucideIcon; className: string }
> = {
  untested: {
    Icon: Circle,
    className: "text-muted-foreground/40",
  },
  testing: {
    Icon: Loader2,
    className: "animate-spin text-blue-500 dark:text-blue-400",
  },
  success: {
    Icon: CircleCheck,
    className: "text-emerald-500 dark:text-emerald-400",
  },
  failed: {
    Icon: CircleX,
    className: "text-red-500 dark:text-red-400",
  },
};

export function ProviderTestStatusIcon({
  providerName,
  status,
  isTesting,
}: ProviderTestStatusIconProps) {
  const { t } = useTranslation();
  const displayStatus: DisplayStatus = isTesting
    ? "testing"
    : (status ?? "untested");
  const { Icon, className } = STATUS_STYLES[displayStatus];

  const label =
    displayStatus === "testing"
      ? t("streamCheck.statusTesting", {
          providerName,
          defaultValue: "{{providerName}}: Testing",
        })
      : displayStatus === "success"
        ? t("streamCheck.statusSuccess", {
            providerName,
            defaultValue: "{{providerName}}: Test passed",
          })
        : displayStatus === "failed"
          ? t("streamCheck.statusFailed", {
              providerName,
              defaultValue: "{{providerName}}: Test failed",
            })
          : t("streamCheck.statusUntested", {
              providerName,
              defaultValue: "{{providerName}}: Not tested",
            });

  return (
    <span
      role="img"
      aria-label={label}
      title={label}
      data-status={displayStatus}
      className={cn(
        "inline-flex h-5 w-5 flex-shrink-0 items-center justify-center",
        className,
      )}
    >
      <Icon className="h-4 w-4" aria-hidden="true" />
    </span>
  );
}
