import { defineConfig } from '@vmz/vmz';

/**
 * The WebView2 host loads the local static delivery from packages/open-zipper/dist/gui.
 * Builtin profile `static` → assembly `web-static`.
 */
export default defineConfig({
    delivery: {
        default: 'static',
        profiles: {
            static: {
                host: 'browser',
                assembly: 'web-static',
            },
        },
    },
});
