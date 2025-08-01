export default function init_pdfium()  {
    return {
        onSuccess: (wasm) => {
            PDFiumModule().then(async pdfiumModule => {
                wasm.initialize_pdfium_render(
                    pdfiumModule, // Emscripten-wrapped Pdfium WASM module
                    wasm, // wasm_bindgen-wrapped WASM module built from our Rust application
                    false, // Debugging flag; set this to true to get tracing information logged to the Javascript console
                )
                console.assert(
                    wasm.init_pdfium(
                        pdfiumModule,
                        wasm,
                        false,
                    ),
                    "Initialization of pdfium-render failed!"
                );
            })
        },
    }

}