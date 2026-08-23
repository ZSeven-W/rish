declare module 'librish_napi.so' {
  interface RishNativeModule {
    planJson(request: string): string;
    executeAppletJson(request: string): string;
    vmRunDockerJson(request: string): string;
    protocolVersion(): number;
  }

  const rish: RishNativeModule;
  export default rish;
}
