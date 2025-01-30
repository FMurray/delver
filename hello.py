# def main():
#     from delver_pdf import process_pdf_file
#     import json

#     output = process_pdf_file("./tests/3M_2015_10K.pdf", "./10K.tmpl")
#     output_json = json.loads(output)
#     print(output_json)


# if __name__ == "__main__":
#     main()

import fitz


def extract_text_with_bounding_boxes(pdf_path, pages=None):
    doc = fitz.open(pdf_path)
    my_range = range(len(doc)) if pages is None else range(pages)
    for page_num in my_range:
        page = doc[page_num]
        text_dict = page.get_text("dict")
        for block in text_dict["blocks"]:
            for line in block["lines"]:
                for span in line["spans"]:
                    text = span["text"]
                    bbox = span["bbox"]
                    print(f"Text: {text}, Bounding Box: {bbox}")
    doc.close()


print(extract_text_with_bounding_boxes("./tests/3M_2015_10K.pdf", 1))
