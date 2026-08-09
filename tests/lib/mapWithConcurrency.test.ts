import { describe, expect, it } from "vitest";
import { mapWithConcurrency } from "@/lib/utils/mapWithConcurrency";

describe("mapWithConcurrency", () => {
  it("preserves result order and never exceeds the concurrency limit", async () => {
    let active = 0;
    let maxActive = 0;

    const results = await mapWithConcurrency(
      [1, 2, 3, 4, 5, 6, 7],
      3,
      async (item) => {
        active += 1;
        maxActive = Math.max(maxActive, active);
        await new Promise((resolve) => setTimeout(resolve, 5));
        active -= 1;
        return item * 2;
      },
    );

    expect(maxActive).toBe(3);
    expect(results).toEqual([2, 4, 6, 8, 10, 12, 14]);
  });

  it("rejects invalid concurrency values", async () => {
    await expect(
      mapWithConcurrency([1], 0, async (item) => item),
    ).rejects.toThrow("concurrency must be a positive integer");
  });
});
