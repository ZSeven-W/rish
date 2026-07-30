/**
 * ArkTS adapter contract for the native rish N-API module.
 *
 * The platform module must expose only allow-listed operations. Returning a
 * host call does not grant permission by itself; ArkTS still performs the
 * platform permission check before dispatch.
 */
import rish from 'librish_ffi.so';

export function planJson(request: string): string {
  return rish.planJson(request);
}
