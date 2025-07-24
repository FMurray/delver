export default async function() {
    const pdfium = await window.PDFiumModule();

    return {
        onSuccess: (wasm) => {
            wasm.initialize_pdfium_render(pdfium, wasm);
        }
    }
} 