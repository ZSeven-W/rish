declare module 'librish_ffi.so' {
  interface RishNativeModule {
    planJson(request: string): string;
    protocolVersion(): number;
  }

  const rish: RishNativeModule;
  export default rish;
}
