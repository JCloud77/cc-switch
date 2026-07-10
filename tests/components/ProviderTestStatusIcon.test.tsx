import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { ProviderTestStatusIcon } from "@/components/providers/ProviderTestStatusIcon";

describe("ProviderTestStatusIcon", () => {
  it("renders neutral, loading, success, and failure states", () => {
    const { rerender } = render(
      <ProviderTestStatusIcon providerName="Provider A" isTesting={false} />,
    );

    let indicator = screen.getByRole("img", {
      name: "Provider A: Not tested",
    });
    expect(indicator).toHaveAttribute("data-status", "untested");
    expect(indicator).toHaveClass("text-muted-foreground/40");

    rerender(<ProviderTestStatusIcon providerName="Provider A" isTesting />);
    indicator = screen.getByRole("img", { name: "Provider A: Testing" });
    expect(indicator).toHaveAttribute("data-status", "testing");
    expect(indicator).toHaveClass("animate-spin", "text-blue-500");

    rerender(
      <ProviderTestStatusIcon
        providerName="Provider A"
        status="success"
        isTesting={false}
      />,
    );
    indicator = screen.getByRole("img", { name: "Provider A: Test passed" });
    expect(indicator).toHaveAttribute("data-status", "success");
    expect(indicator).toHaveClass("text-emerald-500");

    rerender(
      <ProviderTestStatusIcon
        providerName="Provider A"
        status="failed"
        isTesting={false}
      />,
    );
    indicator = screen.getByRole("img", { name: "Provider A: Test failed" });
    expect(indicator).toHaveAttribute("data-status", "failed");
    expect(indicator).toHaveClass("text-red-500");
  });

  it("shows loading instead of a stale previous result while retesting", () => {
    render(
      <ProviderTestStatusIcon
        providerName="Provider B"
        status="success"
        isTesting
      />,
    );

    expect(
      screen.getByRole("img", { name: "Provider B: Testing" }),
    ).toHaveAttribute("data-status", "testing");
  });
});
