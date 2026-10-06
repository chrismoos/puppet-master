// iOS uses automaticallyAdjustKeyboardInsets to scroll focused fields into
// view inside scroll containers. Android does not have this prop and is not
// covered here — this asymmetry is deliberate, not an oversight. The
// root-level KeyboardAvoidingRoot handles the viewport shift on iOS; Android
// relies on windowSoftInputMode="adjustResize" in the manifest.
export function keyboardScrollDefaults(isIos: boolean) {
  return isIos
    ? {
        keyboardShouldPersistTaps: "handled" as const,
        automaticallyAdjustKeyboardInsets: true as const,
      }
    : { keyboardShouldPersistTaps: "handled" as const };
}
