import csv
import sqlite3
from pathlib import Path


def extract(csv_path: str) -> list[dict]:
    with open(csv_path, newline="") as f:
        return list(csv.DictReader(f))


def transform(rows: list[dict]) -> list[dict]:
    cleaned = []
    for row in rows:
        try:
            cleaned.append({
                "id":     int(row["id"]),
                "name":   row["name"].strip().title(),
                "amount": float(row["amount"]),
            })
        except (KeyError, ValueError):
            pass  # drop malformed rows
    return cleaned


def load(rows: list[dict], db_path: str) -> int:
    con = sqlite3.connect(db_path)
    con.execute(
        "CREATE TABLE IF NOT EXISTS records "
        "(id INTEGER PRIMARY KEY, name TEXT, amount REAL)"
    )
    con.executemany(
        "INSERT OR REPLACE INTO records VALUES (:id, :name, :amount)", rows
    )
    con.commit()
    con.close()
    return len(rows)


def run(csv_path: str, db_path: str) -> None:
    rows = extract(csv_path)
    rows = transform(rows)
    n = load(rows, db_path)
    print(f"loaded {n} rows → {db_path}")
