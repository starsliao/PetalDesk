export const PREPARE_UPDATE_INSTALL_EVENT = "petaldesk:prepare-update-install";

export interface PrepareUpdateInstallDetail {
  waitUntil(promise: Promise<unknown>): void;
}

export function addUpdateInstallPreparation(
  handler: () => Promise<unknown> | void,
): () => void {
  const listener = (event: Event): void => {
    const detail = (event as CustomEvent<PrepareUpdateInstallDetail>).detail;
    detail?.waitUntil(Promise.resolve().then(handler));
  };
  window.addEventListener(PREPARE_UPDATE_INSTALL_EVENT, listener);
  return () => window.removeEventListener(PREPARE_UPDATE_INSTALL_EVENT, listener);
}

export async function prepareCurrentWindowForUpdate(): Promise<void> {
  const pending: Promise<unknown>[] = [];
  window.dispatchEvent(new CustomEvent<PrepareUpdateInstallDetail>(PREPARE_UPDATE_INSTALL_EVENT, {
    detail: {
      waitUntil(promise) {
        pending.push(Promise.resolve(promise));
      },
    },
  }));
  await Promise.all(pending);
}
