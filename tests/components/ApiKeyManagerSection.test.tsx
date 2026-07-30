import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { ApiKeyManagerSection } from "@/components/providers/forms/shared/ApiKeyManagerSection";
import type { ApiKeyEntry } from "@/types";

const existingKey: ApiKeyEntry = {
  id: "key-a",
  label: "Key A",
  key: "sk-same-value",
  strategy: "bearer",
};

describe("ApiKeyManagerSection", () => {
  it("shows the Claude/Codex central-pool notice", () => {
    render(
      <ApiKeyManagerSection
        apiKeys={[existingKey]}
        selectedKeyId="key-a"
        currentKeyValue=""
        sharedKeyApps={["claude", "codex"]}
        onApiKeysChange={vi.fn()}
        onSelectedKeyIdChange={vi.fn()}
      />,
    );

    expect(screen.getByText(/Claude.*Codex/i)).toBeInTheDocument();
  });

  it("selects an exact duplicate instead of saving it twice", async () => {
    const user = userEvent.setup();
    const onApiKeysChange = vi.fn();
    const onSelectedKeyIdChange = vi.fn();
    render(
      <ApiKeyManagerSection
        apiKeys={[existingKey]}
        currentKeyValue="  sk-same-value  "
        onApiKeysChange={onApiKeysChange}
        onSelectedKeyIdChange={onSelectedKeyIdChange}
      />,
    );

    await user.click(screen.getByRole("button", { name: /保存|save/i }));

    expect(onApiKeysChange).not.toHaveBeenCalled();
    expect(onSelectedKeyIdChange).toHaveBeenCalledWith("key-a");
  });

  it("adds a distinct key with the requested strategy", async () => {
    const user = userEvent.setup();
    const onApiKeysChange = vi.fn();
    render(
      <ApiKeyManagerSection
        apiKeys={[existingKey]}
        currentKeyValue="sk-new-value"
        newKeyStrategy="claude_auth"
        onApiKeysChange={onApiKeysChange}
        onSelectedKeyIdChange={vi.fn()}
      />,
    );

    await user.click(screen.getByRole("button", { name: /保存|save/i }));

    expect(onApiKeysChange).toHaveBeenCalledTimes(1);
    const nextKeys = onApiKeysChange.mock.calls[0][0] as ApiKeyEntry[];
    expect(nextKeys).toHaveLength(2);
    expect(nextKeys[1]).toMatchObject({
      key: "sk-new-value",
      strategy: "claude_auth",
    });
  });
});
