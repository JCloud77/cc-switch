import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import {
  ProviderTestStatusProvider,
  useProviderTestStatus,
} from "@/contexts/ProviderTestStatusContext";
import type { AppId } from "@/lib/api";

function StatusConsumer({ appId }: { appId: AppId }) {
  const { setChecking, setStatus, isChecking, getStatus } =
    useProviderTestStatus();
  const providerId = "shared-provider-id";

  return (
    <div>
      <output data-testid={`${appId}-status`}>
        {getStatus(appId, providerId) ?? "untested"}
      </output>
      <output data-testid={`${appId}-checking`}>
        {isChecking(appId, providerId) ? "checking" : "idle"}
      </output>
      <button onClick={() => setStatus(appId, providerId, "success")}>
        pass-{appId}
      </button>
      <button onClick={() => setStatus(appId, providerId, "failed")}>
        fail-{appId}
      </button>
      <button onClick={() => setChecking(appId, providerId, true)}>
        start-{appId}
      </button>
    </div>
  );
}

function RemountHarness() {
  const [visible, setVisible] = useState(true);
  return (
    <ProviderTestStatusProvider>
      <button onClick={() => setVisible((previous) => !previous)}>
        toggle
      </button>
      {visible && <StatusConsumer appId="claude" />}
    </ProviderTestStatusProvider>
  );
}

describe("ProviderTestStatusContext", () => {
  it("retains result and in-flight state when a consumer remounts", async () => {
    const user = userEvent.setup();
    render(<RemountHarness />);

    await user.click(screen.getByRole("button", { name: "pass-claude" }));
    await user.click(screen.getByRole("button", { name: "start-claude" }));
    await user.click(screen.getByRole("button", { name: "toggle" }));
    await user.click(screen.getByRole("button", { name: "toggle" }));

    expect(screen.getByTestId("claude-status")).toHaveTextContent("success");
    expect(screen.getByTestId("claude-checking")).toHaveTextContent("checking");
  });

  it("isolates providers with the same id by app", async () => {
    const user = userEvent.setup();
    render(
      <ProviderTestStatusProvider>
        <StatusConsumer appId="claude" />
        <StatusConsumer appId="codex" />
      </ProviderTestStatusProvider>,
    );

    await user.click(screen.getByRole("button", { name: "pass-claude" }));
    await user.click(screen.getByRole("button", { name: "fail-codex" }));

    expect(screen.getByTestId("claude-status")).toHaveTextContent("success");
    expect(screen.getByTestId("codex-status")).toHaveTextContent("failed");
  });
});
