export default function() {
    return {
        onStart: () => {
            console.log("onStart");

        },
        onSuccess: (wasm) => {
            console.log("onSuccess");
            console.log(wasm);
            PDFiumModule().then(async pdfiumModule => {
                console.log("pdfiumModule", pdfiumModule);
                wasm.initialize_pdfium_render(pdfiumModule, wasm);
            })
            //     console.log("pdfiumModule", pdfiumModule);
            //     wasm.initialize_pdfium_render(pdfiumModule, wasm);
            // })
        },
    }
} 