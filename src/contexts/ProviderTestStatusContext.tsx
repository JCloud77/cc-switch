import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import type { AppId } from "@/lib/api";

export type ProviderTestStatus = "success" | "failed";

interface ProviderTestStatusContextValue {
  setChecking: (appId: AppId, providerId: string, checking: boolean) => void;
  setStatus: (
    appId: AppId,
    providerId: string,
    status: ProviderTestStatus,
  ) => void;
  isChecking: (appId: AppId, providerId: string) => boolean;
  isCheckingAny: (appId: AppId) => boolean;
  getStatus: (
    appId: AppId,
    providerId: string,
  ) => ProviderTestStatus | undefined;
}

const ProviderTestStatusContext = createContext<
  ProviderTestStatusContextValue | undefined
>(undefined);

const providerKey = (appId: AppId, providerId: string) =>
  `${appId}:${providerId}`;

export function ProviderTestStatusProvider({
  children,
}: {
  children: ReactNode;
}) {
  const [checkingKeys, setCheckingKeys] = useState<Set<string>>(new Set());
  const [statuses, setStatuses] = useState<Map<string, ProviderTestStatus>>(
    new Map(),
  );

  const setChecking = useCallback(
    (appId: AppId, providerId: string, checking: boolean) => {
      const key = providerKey(appId, providerId);
      setCheckingKeys((previous) => {
        const next = new Set(previous);
        if (checking) {
          next.add(key);
        } else {
          next.delete(key);
        }
        return next;
      });
    },
    [],
  );

  const setStatus = useCallback(
    (appId: AppId, providerId: string, status: ProviderTestStatus) => {
      const key = providerKey(appId, providerId);
      setStatuses((previous) => new Map(previous).set(key, status));
    },
    [],
  );

  const isChecking = useCallback(
    (appId: AppId, providerId: string) =>
      checkingKeys.has(providerKey(appId, providerId)),
    [checkingKeys],
  );

  const isCheckingAny = useCallback(
    (appId: AppId) => {
      const prefix = `${appId}:`;
      return [...checkingKeys].some((key) => key.startsWith(prefix));
    },
    [checkingKeys],
  );

  const getStatus = useCallback(
    (appId: AppId, providerId: string) =>
      statuses.get(providerKey(appId, providerId)),
    [statuses],
  );

  const value = useMemo(
    () => ({
      setChecking,
      setStatus,
      isChecking,
      isCheckingAny,
      getStatus,
    }),
    [getStatus, isChecking, isCheckingAny, setChecking, setStatus],
  );

  return (
    <ProviderTestStatusContext.Provider value={value}>
      {children}
    </ProviderTestStatusContext.Provider>
  );
}

export function useProviderTestStatus() {
  const context = useContext(ProviderTestStatusContext);
  if (!context) {
    throw new Error(
      "useProviderTestStatus must be used within ProviderTestStatusProvider",
    );
  }
  return context;
}
