use rust_xlsxwriter::{Format, Workbook};

use crate::models::errors::EluEtlError;
use crate::models::value::CellValue;

/// 將查詢結果寫出為 xlsx（表頭粗體、日期含格式）。
pub fn write_xlsx(
    path: &str,
    columns: &[String],
    rows: &[Vec<CellValue>],
) -> Result<u64, EluEtlError> {
    let mut workbook = Workbook::new();
    let sheet = workbook.add_worksheet();

    let bold = Format::new().set_bold();
    let datetime_fmt = Format::new().set_num_format("yyyy-mm-dd hh:mm:ss");
    let date_fmt = Format::new().set_num_format("yyyy-mm-dd");

    for (c, name) in columns.iter().enumerate() {
        sheet.write_string_with_format(0, c as u16, name, &bold)?;
    }

    for (r, row) in rows.iter().enumerate() {
        let r = (r + 1) as u32;
        for (c, cell) in row.iter().enumerate() {
            let c = c as u16;
            match cell {
                CellValue::Null => {}
                CellValue::Int(v) => {
                    sheet.write_number(r, c, *v as f64)?;
                }
                CellValue::Float(v) => {
                    sheet.write_number(r, c, *v)?;
                }
                CellValue::Text(v) => {
                    sheet.write_string(r, c, v)?;
                }
                CellValue::Bool(v) => {
                    sheet.write_boolean(r, c, *v)?;
                }
                CellValue::DateTime(v) => {
                    sheet.write_datetime_with_format(r, c, v, &datetime_fmt)?;
                }
                CellValue::Date(v) => {
                    sheet.write_datetime_with_format(r, c, v, &date_fmt)?;
                }
            }
        }
    }

    workbook.save(path)?;
    Ok(rows.len() as u64)
}
