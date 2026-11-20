//! Host tests for Solon's spreadsheet engine (src/doc): formulas, number formats, dates,
//! xlsx and csv round trips. Usage: doctest [out-folder]  (writes sample files there)

extern crate alloc;

/// Stand-ins for the kernel pieces the engine uses.
mod rtc {
    pub fn fat_timestamp() -> u32 {
        (46 << 25) | (11 << 21) | (20 << 16) | (12 << 11)
    }
    pub fn date() -> (u16, u8, u8) {
        (2026, 11, 20)
    }
}
mod fs {
    pub mod part {
        pub fn crc32(data: &[u8]) -> u32 {
            let mut c = 0xFFFF_FFFFu32;
            for &b in data {
                c ^= b as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
                }
            }
            !c
        }
    }
}

#[path = "../../../src/doc/mod.rs"]
#[allow(dead_code)]
mod doc;

use doc::formula::eval_text;
use doc::sheet::{Content, NumFmt, Workbook};

static mut FAILS: u32 = 0;

fn check(what: &str, got: &str, want: &str) {
    if got == want {
        println!("ok   {:<40} {}", what, got);
    } else {
        println!("FAIL {:<40} got {:?}, want {:?}", what, got, want);
        unsafe { FAILS += 1 };
    }
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("pattern") {
        // FNV-1a of the test phone's pretend video (src/phone.rs), to check imports.
        let mut h: u32 = 0x811C_9DC5;
        for i in 0..20 * 1024 * 1024 + 12345u32 {
            h = (h ^ (i.wrapping_mul(2_654_435_761) >> 24)).wrapping_mul(0x0100_0193);
        }
        println!("test phone video: {} bytes fnv {:08x}", 20 * 1024 * 1024 + 12345, h);
        return;
    }
    let out = std::env::args().nth(1);
    let mut wb = Workbook::new();
    let s = 0;
    for (cell, input) in [
        ("A1", "10"),
        ("A2", "20"),
        ("A3", "30.5"),
        ("A4", "apples"),
        ("B1", "=A1+A2*2"),
        ("B2", "=SUM(A1:A4)"),
        ("B3", "=AVERAGE(A1:A3)"),
        ("B4", "=IF(A1>5,\"big\",\"small\")"),
        ("B5", "=B6+1"),
        ("B6", "=B5+1"),
        ("B7", "=A1/0"),
        ("B8", "=ROUND(2/3,2)"),
        ("B9", "=CONCATENATE(A4,\" x\",A1)"),
        ("B10", "=COUNTIF(A1:A4,\">15\")"),
        ("B11", "=SUMIF(A1:A3,\">=20\")"),
        ("B12", "=Sheet2!A1*2"),
        ("B13", "=-A1^2"),
        ("B14", "=50%*A2"),
        ("B15", "=FOO(1)"),
        ("C1", "$1,234.50"),
        ("C2", "45%"),
        ("C3", "11/20/2026"),
        ("C4", "1,000"),
        ("C5", "=C3+10"),
        ("C6", "=COUNT(A1:A4)&\"/\"&COUNTA(A1:A4)"),
        ("C7", "=MAX(A1:A3)-MIN(A1:A3)"),
        ("C8", "=AND(A1>1,A2>1)"),
    ] {
        let (r, c) = doc::sheet::parse_cell(cell).unwrap();
        wb.set_input(s, r, c, input);
    }
    wb.sheets.push(doc::sheet::Sheet::new("Sheet2"));
    wb.set_input(1, 0, 0, "21");
    wb.recalc();
    let d = |wb: &Workbook, cell: &str| {
        let (r, c) = doc::sheet::parse_cell(cell).unwrap();
        wb.display(0, r, c)
    };
    check("A1+A2*2", &d(&wb, "B1"), "50");
    check("SUM skips text", &d(&wb, "B2"), "60.5");
    check("AVERAGE", &d(&wb, "B3"), "20.16666667");
    check("IF", &d(&wb, "B4"), "big");
    check("cycle", &d(&wb, "B5"), "#CYCLE!");
    check("div by zero", &d(&wb, "B7"), "#DIV/0!");
    check("ROUND", &d(&wb, "B8"), "0.67");
    check("CONCATENATE", &d(&wb, "B9"), "apples x10");
    check("COUNTIF", &d(&wb, "B10"), "2");
    check("SUMIF", &d(&wb, "B11"), "50.5");
    check("other sheet", &d(&wb, "B12"), "42");
    check("-A1^2 (Excel: 100)", &d(&wb, "B13"), "100");
    check("50%*A2", &d(&wb, "B14"), "10");
    check("unknown function", &d(&wb, "B15"), "#NAME?");
    check("money typed", &d(&wb, "C1"), "$1,234.50");
    check("percent typed", &d(&wb, "C2"), "45%");
    check("date typed", &d(&wb, "C3"), "11/20/2026");
    check("thousands typed", &d(&wb, "C4"), "1,000");
    check("date + 10 (general)", &d(&wb, "C5"), "46356");
    check("COUNT/COUNTA", &d(&wb, "C6"), "3/4");
    check("MAX-MIN", &d(&wb, "C7"), "20.5");
    check("AND", &d(&wb, "C8"), "TRUE");
    check("date serial 2026-11-20", &format!("{}", doc::sheet::date_serial(2026, 11, 20)), "46346");
    check("general 1/3 (11 chars, like Excel)", &doc::sheet::general(1.0 / 3.0), "0.333333333");
    check("general big", &doc::sheet::general(123456789012.0), "1.23457E+11");
    check("general 0.1+0.2", &doc::sheet::general(0.1 + 0.2), "0.3");
    check("fixed thousands", &doc::sheet::fixed(-1234567.891, 2, true), "-1,234,567.89");
    check("normalize", &doc::formula::normalize("sum(a1:b2)&\"abc\"+Sheet2!a1+'my sheet'!b$3"), "SUM(A1:B2)&\"abc\"+Sheet2!A1+'my sheet'!B$3");
    check("eval text", &eval_text(&wb, 0, "LEN(\"hello\")+1"), "6");
    check("from_code money", NumFmt::from_code("\"$\"#,##0.00").label(), "Money");
    check("from_code custom date", &NumFmt::from_code("d-mmm-yy").show(46346.0), "11/20/2026");
    check("from_code accounting", &NumFmt::from_code("_(\"$\"* #,##0.00_)").show(1234.5), "$1,234.50");

    // Style a few cells, set a width, then write and read back.
    wb.restyle(0, 0, 0, 0, 1, &|st| st.bold = true);
    wb.restyle(0, 1, 0, 1, 0, &|st| {
        st.fill = Some(0xFFFF00);
        st.color = Some(0xC00000);
    });
    wb.sheets[0].widths.insert(1, 20.0);
    let bytes = doc::xlsx::write(&wb);
    let back = doc::xlsx::read(&bytes).expect("read back our own xlsx");
    check("round trip sheets", &back.sheets.len().to_string(), "2");
    check("round trip B2", &d(&back, "B2"), "60.5");
    check("round trip C1 money", &d(&back, "C1"), "$1,234.50");
    check("round trip C3 date", &d(&back, "C3"), "11/20/2026");
    check("round trip bold", &back.sheets[0].style(0, 0).bold.to_string(), "true");
    check("round trip fill", &format!("{:06X}", back.sheets[0].style(1, 0).fill.unwrap_or(0)), "FFFF00");
    check("round trip color", &format!("{:06X}", back.sheets[0].style(1, 0).color.unwrap_or(0)), "C00000");
    check("round trip width", &format!("{:.1}", back.sheets[0].width(1)), "20.0");
    check("round trip formula text", &match &back.sheets[0].cells[&(1, 1)].content {
        Content::Formula(f, _) => f.clone(),
        _ => "not a formula".into(),
    }, "SUM(A1:A4)");
    check("round trip cross-sheet", &d(&back, "B12"), "42");

    // CSV
    let csv = doc::xlsx::write_csv(&wb);
    let csv_text = String::from_utf8(csv.clone()).unwrap();
    check("csv first line", csv_text.lines().next().unwrap(), "10,50,\"$1,234.50\"");
    let wb2 = doc::xlsx::read_csv(b"name,amount\r\n\"Smith, J\",12.5\r\nDoe,=B2*2\r\n\"say \"\"hi\"\"\",7\r\n");
    check("csv quoted comma", &wb2.display(0, 1, 0), "Smith, J");
    check("csv number", &wb2.display(0, 1, 1), "12.5");
    check("csv '=' stays text-free", &wb2.display(0, 2, 1), "B2*2");
    check("csv doubled quotes", &wb2.display(0, 3, 0), "say \"hi\"");

    // A hand-made workbook in Excel's own style: shared strings, shared formulas,
    // styles with built-in formats, and a sheet relationship that isn't sheet1.xml.
    let excel = excel_style_sample();
    let ex = doc::xlsx::read(&excel).expect("read Excel-style sample");
    check("excel: sheet names", &ex.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>().join(","), "Budget,Notes");
    check("excel: shared string", &ex.display(0, 0, 0), "Item");
    check("excel: rich shared string", &ex.display(0, 1, 0), "Rent and power");
    check("excel: number fmt 4", &ex.display(0, 1, 1), "1,200.00");
    check("excel: shared formula row 3", &match &ex.sheets[0].cells[&(2, 2)].content {
        Content::Formula(f, _) => f.clone(),
        _ => "none".into(),
    }, "B3*12");
    check("excel: shared formula value", &ex.display(0, 2, 2), "1200");
    check("excel: date fmt 14", &ex.display(0, 3, 1), "11/20/2026");
    check("excel: percent fmt 10", &ex.display(0, 4, 1), "12.50%");
    check("excel: bold font", &ex.sheets[0].style(0, 0).bold.to_string(), "true");
    check("excel: unknown fn keeps cached", &ex.display(0, 5, 1), "99");
    check("excel: notes sheet", &ex.display(1, 0, 0), "hello & <bye>");

    if let Some(dir) = out {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(format!("{}/solon-written.xlsx", dir), &bytes).unwrap();
        std::fs::write(format!("{}/excel-style.xlsx", dir), &excel).unwrap();
        std::fs::write(format!("{}/solon-written.csv", dir), &csv).unwrap();
        println!("wrote samples to {}", dir);
    }
    let fails = unsafe { FAILS };
    println!("{}", if fails == 0 { "ALL PASSED".to_string() } else { format!("{} FAILED", fails) });
    std::process::exit(if fails == 0 { 0 } else { 1 });
}

fn excel_style_sample() -> Vec<u8> {
    let types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#;
    let workbook = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006"><fileVersion appName="xl" lastEdited="7"/><bookViews><workbookView xWindow="0" yWindow="0"/></bookViews><sheets><sheet name="Budget" sheetId="1" r:id="rId3"/><sheet name="Notes" sheetId="2" r:id="rId1"/></sheets><calcPr calcId="191029"/></workbook>"#;
    let wrels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet2.xml"/><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="/xl/worksheets/sheet1.xml"/><Relationship Id="rId4" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/><Relationship Id="rId5" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#;
    let shared = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="4" uniqueCount="4"><si><t>Item</t></si><si><r><rPr><b/></rPr><t>Rent</t></r><r><t xml:space="preserve"> and power</t></r></si><si><t>Amount</t></si><si><t>hello &amp; &lt;bye&gt;</t></si></sst>"#;
    let styles = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><fonts count="2"><font><sz val="11"/><color theme="1"/><name val="Calibri"/></font><font><b/><sz val="11"/><color theme="1"/><name val="Calibri"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="5"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/><xf numFmtId="4" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="14" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"/><xf numFmtId="10" fontId="0" fillId="0" borderId="0" xfId="0" applyNumberFormat="1"><alignment horizontal="center"/></xf></cellXfs></styleSheet>"#;
    let sheet_budget = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" xmlns:x14ac="http://schemas.microsoft.com/office/spreadsheetml/2009/9/ac"><dimension ref="A1:C6"/><sheetViews><sheetView tabSelected="1" workbookViewId="0"/></sheetViews><sheetFormatPr defaultRowHeight="15" x14ac:dyDescent="0.25"/><cols><col min="1" max="1" width="18.7109375" customWidth="1"/></cols><sheetData><row r="1" spans="1:3"><c r="A1" s="1" t="s"><v>0</v></c><c r="B1" s="1" t="s"><v>2</v></c></row><row r="2" spans="1:3"><c r="A2" t="s"><v>1</v></c><c r="B2" s="2"><v>1200</v></c><c r="C2"><f t="shared" ref="C2:C3" si="0">B2*12</f><v>14400</v></c></row><row r="3"><c r="B3"><v>100</v></c><c r="C3"><f t="shared" si="0"/><v>1200</v></c></row><row r="4"><c r="B4" s="3"><v>46346</v></c></row><row r="5"><c r="B5" s="4"><v>0.125</v></c></row><row r="6"><c r="B6"><f>_xlfn.XLOOKUP(1,A1:A2,B1:B2)</f><v>99</v></c></row></sheetData><pageMargins left="0.7" right="0.7" top="0.75" bottom="0.75" header="0.3" footer="0.3"/></worksheet>"#;
    let sheet_notes = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="s"><v>3</v></c></row></sheetData></worksheet>"#;
    doc::zip::write(&[
        ("[Content_Types].xml", types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("xl/workbook.xml", workbook.as_bytes()),
        ("xl/_rels/workbook.xml.rels", wrels.as_bytes()),
        ("xl/sharedStrings.xml", shared.as_bytes()),
        ("xl/styles.xml", styles.as_bytes()),
        ("xl/worksheets/sheet2.xml", sheet_budget.as_bytes()),
        ("xl/worksheets/sheet1.xml", sheet_notes.as_bytes()),
    ])
}
