import fitz

doc = fitz.open("test.pdf")

for page in doc:
    print(page.get_text())
