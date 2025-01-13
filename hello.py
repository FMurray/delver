def main():
    from delver_pdf import process_pdf_file
    import json

    output = process_pdf_file("./tests/3M_2015_10K.pdf", "./10K.tmpl")
    output_json = json.loads(output)
    print(output_json)


if __name__ == "__main__":
    main()
