import { useState } from "react";
import { useTranslation } from "react-i18next";
import { CircleCheck, Plus, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { ApiKeyEntry } from "@/types";
import { cn } from "@/lib/utils";

interface ApiKeyManagerSectionProps {
  /** Currently saved keys */
  apiKeys: ApiKeyEntry[];
  /** Currently selected key id */
  selectedKeyId?: string;
  /** Current value in the main API key input */
  currentKeyValue: string;
  /** Called when keys list changes */
  onApiKeysChange: (keys: ApiKeyEntry[]) => void;
  /** Called when selected key changes */
  onSelectedKeyIdChange: (id: string | undefined) => void;
  /** Auth strategy for new keys */
  defaultStrategy?: string;
  /** Auth strategy for new keys (Codex always "bearer") */
  newKeyStrategy?: string;
  disabled?: boolean;
}

export function ApiKeyManagerSection({
  apiKeys,
  selectedKeyId,
  currentKeyValue,
  onApiKeysChange,
  onSelectedKeyIdChange,
  defaultStrategy,
  newKeyStrategy,
  disabled,
}: ApiKeyManagerSectionProps) {
  const { t } = useTranslation();
  const [newKeyLabel, setNewKeyLabel] = useState("");

  const handleAddKey = () => {
    if (!currentKeyValue.trim()) return;
    const label = newKeyLabel.trim() || `Key ${apiKeys.length + 1}`;
    const newEntry: ApiKeyEntry = {
      id: crypto.randomUUID(),
      label,
      key: currentKeyValue.trim(),
      strategy: newKeyStrategy || defaultStrategy || "bearer",
    };
    onApiKeysChange([...apiKeys, newEntry]);
    onSelectedKeyIdChange(newEntry.id);
    setNewKeyLabel("");
  };

  const handleDeleteKey = (id: string) => {
    const next = apiKeys.filter((k) => k.id !== id);
    onApiKeysChange(next);
    if (selectedKeyId === id) {
      onSelectedKeyIdChange(next.length > 0 ? next[0].id : undefined);
    }
  };

  return (
    <div className="space-y-3 border-t border-border-default pt-3">
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-medium">
          {t("providerForm.savedKeys", { defaultValue: "备用 API Key" })}
        </span>
      </div>

      {/* Saved keys list */}
      {apiKeys.length > 0 && (
        <div className="space-y-1">
          {apiKeys.map((entry) => {
            const isSelected = entry.id === selectedKeyId;
            return (
              <div
                key={entry.id}
                className={cn(
                  "flex items-center gap-2 rounded-md border p-2 transition-colors",
                  isSelected
                    ? "border-primary/50 bg-primary/5"
                    : "border-border-default",
                )}
              >
                <button
                  type="button"
                  className={cn(
                    "flex min-w-0 flex-1 items-center gap-2 text-left text-sm",
                  )}
                  onClick={() => onSelectedKeyIdChange(entry.id)}
                >
                  <CircleCheck
                    className={cn(
                      "h-4 w-4 shrink-0",
                      isSelected ? "text-primary" : "text-muted-foreground",
                    )}
                  />
                  <span className="min-w-0 truncate font-medium">
                    {entry.label}
                  </span>
                  <span className="min-w-0 truncate text-xs text-muted-foreground">
                    {entry.key.slice(0, 8)}...
                    {entry.key.slice(-4)}
                  </span>
                </button>
                <Button
                  type="button"
                  variant="ghost"
                  size="icon"
                  className="h-7 w-7 shrink-0 text-muted-foreground hover:text-destructive"
                  onClick={() => handleDeleteKey(entry.id)}
                  disabled={disabled}
                >
                  <Trash2 className="h-3.5 w-3.5" />
                </Button>
              </div>
            );
          })}
        </div>
      )}

      {/* Add current key to saved */}
      {currentKeyValue.trim() && (
        <div className="flex items-center gap-2">
          <Input
            value={newKeyLabel}
            onChange={(e) => setNewKeyLabel(e.target.value)}
            placeholder={t("providerForm.keyLabelPlaceholder", {
              defaultValue: "标签（可选，例如: Key A - opus）",
            })}
            className="h-8 flex-1 text-xs"
            disabled={disabled}
          />
          <Button
            type="button"
            variant="outline"
            size="sm"
            className="h-8 shrink-0 gap-1 text-xs"
            onClick={handleAddKey}
            disabled={disabled}
          >
            <Plus className="h-3 w-3" />
            {t("providerForm.saveKey", { defaultValue: "保存" })}
          </Button>
        </div>
      )}

      {apiKeys.length === 0 && !currentKeyValue.trim() && (
        <p className="text-xs text-muted-foreground">
          {t("providerForm.noSavedKeysHint", {
            defaultValue:
              "在上方输入 API Key 后，可点击“保存”将其存入 Key 列表，支持多个 Key 之间手动切换。",
          })}
        </p>
      )}
    </div>
  );
}
