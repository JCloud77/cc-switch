import { act, renderHook } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { useStreamCheck } from "@/hooks/useStreamCheck";
import type { StreamCheckResult } from "@/lib/api/model-test";

const streamCheckProviderMock = vi.hoisted(() => vi.fn());

vi.mock("@/lib/api/model-test", () => ({
  streamCheckProvider: (...args: unknown[]) => streamCheckProviderMock(...args),
}));

vi.mock("sonner", () => ({
  toast: {
    success: vi.fn(),
    warning: vi.fn(),
    error: vi.fn(),
  },
}));

function createResult(
  overrides: Partial<StreamCheckResult> = {},
): StreamCheckResult {
  return {
    status: "operational",
    success: true,
    message: "Reachable",
    responseTimeMs: 100,
    testedAt: 1,
    retryCount: 0,
    ...overrides,
  };
}

describe("useStreamCheck provider status", () => {
  beforeEach(() => {
    streamCheckProviderMock.mockReset();
  });

  it("retains mixed outcomes when checks run concurrently", async () => {
    streamCheckProviderMock.mockImplementation(
      (_appId: string, providerId: string) =>
        Promise.resolve(
          providerId === "success"
            ? createResult()
            : createResult({
                status: "failed",
                success: false,
                message: "Unauthorized",
              }),
        ),
    );
    const { result } = renderHook(() => useStreamCheck("claude"));

    await act(async () => {
      await Promise.all([
        result.current.checkProvider("success", "Provider A"),
        result.current.checkProvider("failed", "Provider B"),
      ]);
    });

    expect(result.current.getTestStatus("success")).toBe("success");
    expect(result.current.getTestStatus("failed")).toBe("failed");
    expect(result.current.isCheckingAny).toBe(false);
  });

  it("records thrown check errors as failures", async () => {
    streamCheckProviderMock.mockRejectedValue(new Error("network error"));
    const { result } = renderHook(() => useStreamCheck("codex"));

    await act(async () => {
      await result.current.checkProvider("provider-1", "Provider A");
    });

    expect(result.current.getTestStatus("provider-1")).toBe("failed");
  });
});
