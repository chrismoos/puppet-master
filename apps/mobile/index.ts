import { registerRootComponent } from "expo";

import { installTextCodecPolyfill } from "./src/polyfill/textCodec";
import App from "./App";

installTextCodecPolyfill();
registerRootComponent(App);
