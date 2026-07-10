import { useState, useCallback } from "react";
import { toast } from "sonner";
import { useTranslation } from "react-i18next";
import {
  streamCheckProvider,
  type StreamCheckResult,
} from "@/lib/api/model-test";
import type { AppId } from "@/lib/api";

/**
 * 供应商连通性检查。
 *
 * 先探测 base_url 是否可达；Claude/Codex 会继续发送极小的 agent 风格真实请求。
 * 刻意 **不** 重置故障转移熔断器——可达/测试通过都不应主动把供应商切回线上。
 * 熔断器只由真实转发流量驱动（见 proxy/forwarder.rs）。
 */
export function useStreamCheck(appId: AppId) {
  const { t } = useTranslation();
  const [checkingIds, setCheckingIds] = useState<Set<string>>(new Set());

  const checkProvider = useCallback(
    async (
      providerId: string,
      providerName: string,
    ): Promise<StreamCheckResult | null> => {
      setCheckingIds((prev) => new Set(prev).add(providerId));

      try {
        const result = await streamCheckProvider(appId, providerId);

        if (result.status === "operational") {
          toast.success(
            t("streamCheck.reachable", {
              providerName: providerName,
              responseTimeMs: result.responseTimeMs,
              defaultValue: `${providerName} 连通正常 (${result.responseTimeMs}ms)`,
            }),
            { closeButton: true },
          );
        } else if (result.status === "degraded") {
          toast.warning(
            t("streamCheck.reachableSlow", {
              providerName: providerName,
              responseTimeMs: result.responseTimeMs,
              defaultValue: `${providerName} 连通但较慢 (${result.responseTimeMs}ms)`,
            }),
          );
        } else {
          const isAgentProbeFailure =
            result.errorCategory?.startsWith("agent_");
          toast.error(
            t(
              isAgentProbeFailure
                ? "streamCheck.agentProbeFailed"
                : "streamCheck.unreachable",
              {
                providerName: providerName,
                message: result.message,
                defaultValue: `${providerName} 检测未通过: ${result.message}`,
              },
            ),
            {
              description: t(
                isAgentProbeFailure
                  ? "streamCheck.agentProbeHint"
                  : "streamCheck.unreachableHint",
                {
                  defaultValue: isAgentProbeFailure
                    ? "Base URL 可达，但模拟 Claude/Codex agent 的最小真实请求失败。请检查 API Key、模型名、协议格式或供应商客户端限制。"
                    : "无法建立连接（DNS / 连接 / TLS / 超时）。请检查 base_url 与网络。",
                },
              ),
              duration: 8000,
              closeButton: true,
            },
          );
        }

        return result;
      } catch (e) {
        toast.error(
          t("streamCheck.error", {
            providerName: providerName,
            error: String(e),
            defaultValue: `${providerName} 检查出错: ${String(e)}`,
          }),
        );
        return null;
      } finally {
        setCheckingIds((prev) => {
          const next = new Set(prev);
          next.delete(providerId);
          return next;
        });
      }
    },
    [appId, t],
  );

  const isChecking = useCallback(
    (providerId: string) => checkingIds.has(providerId),
    [checkingIds],
  );

  return {
    checkProvider,
    isChecking,
    isCheckingAny: checkingIds.size > 0,
  };
}
