// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The OCX Authors

/// Prints a table of strings to stdout, with columns aligned based on the longest cell in each column.
pub fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    let mut max_len = Vec::with_capacity(headers.len());
    for header in headers {
        max_len.push(header.len());
    }
    if rows.len() > headers.len() {
        max_len.resize(rows.len(), 0);
    }

    for i in 0..rows.len() {
        for cell in rows[i].iter() {
            if cell.len() > max_len[i] {
                max_len[i] = cell.len();
            }
        }
    }

    for (i, header) in headers.iter().enumerate() {
        print!("{:width$} ", header, width = max_len[i]);
    }
    println!();
    let max_rows = rows.iter().map(|r| r.len()).max().unwrap_or(0);

    for i in 0..max_rows {
        for (j, row) in rows.iter().enumerate() {
            let cell = row.get(i).map_or("", |c| c.as_str());
            print!("{:width$} ", cell, width = max_len[j]);
        }
        println!();
    }
}
