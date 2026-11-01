"""Hidden acceptance cases for the real multi-turn SQL task.

The agent never sees this file. After each turn the test runs it as
`python3 -I minisql_acceptance.py <workspace> <turn>` against the minisql
the agent wrote, and it prints one JSON object scoring the cases of every
turn so far, grouped by the turn that asked for them ("case" is the CASE
expression the user adds while turn 3 runs).

Python's own sqlite3 is the oracle: each case runs on a fresh SQLite database
and on a fresh minisql one, statement by statement. Where SQLite fails,
minisql must raise minisql.Error; where SQLite returns rows, minisql must
return the same rows, in order only under ORDER BY, with SQLite's types
(int, float, str or None; floats within 1e-9). A statement marked "!" is
one SQLite rejects; a case whose oracle disagrees with its marks is broken
and listed apart. REOPEN closes the connection, imports minisql afresh and
connects to the same file again.
"""
import importlib
import json
import math
import os
import random
import re
import shutil
import sys
import tempfile
import time

REOPEN = "REOPEN"
TURN = {"1": 1, "2": 2, "3": 3, "case": 3, "4": 4, "5": 5}

PEOPLE = [
    "CREATE TABLE people (id INTEGER, name TEXT, age INTEGER, city TEXT, score REAL)",
    "INSERT INTO people VALUES (1, 'Alice', 30, 'Paris', 88.5), (2, 'Bob', 25, 'London', 72.0),"
    " (3, 'Carol', 35, 'Paris', 91.25), (4, 'Dave', 40, NULL, 65.5), (5, 'Eve', 28, 'Berlin', NULL),"
    " (6, 'Frank', NULL, 'London', 80.0), (7, 'Grace', 33, 'Berlin', 77.75)",
]
SALES = [
    "CREATE TABLE sales (id INTEGER, region TEXT, product TEXT, qty INTEGER, price REAL)",
    "INSERT INTO sales VALUES (1, 'north', 'apple', 10, 1.5), (2, 'north', 'pear', 5, 2.0),"
    " (3, 'south', 'apple', 7, 1.5), (4, 'south', 'apple', 3, 1.75), (5, 'east', 'pear', NULL, 2.25),"
    " (6, 'east', 'plum', 12, 0.5), (7, 'north', 'plum', 4, 0.75), (8, 'west', 'apple', 6, NULL)",
]
ORDERS = PEOPLE + [
    "CREATE TABLE orders (id INTEGER, person_id INTEGER, item TEXT, amount INTEGER)",
    "INSERT INTO orders VALUES (1, 1, 'book', 12), (2, 1, 'pen', 3), (3, 2, 'lamp', 40),"
    " (4, 3, 'book', 15), (5, 3, 'desk', 120), (6, 3, 'pen', 2), (7, 9, 'chair', 60)",
]
CITIES = ORDERS + [
    "CREATE TABLE cities (name TEXT, country TEXT)",
    "INSERT INTO cities VALUES ('Paris', 'France'), ('London', 'UK'), ('Berlin', 'Germany')",
]
THOUSAND = "INSERT INTO n VALUES " + ", ".join(f"({i}, 'v{i}')" for i in range(1000))

CASES = [
    # Turn 1: tables, inserts and expressions.
    ("1", "select star", PEOPLE + ["SELECT * FROM people"]),
    ("1", "select columns", PEOPLE + ["SELECT name, age FROM people"]),
    ("1", "where equals", PEOPLE + ["SELECT name FROM people WHERE city = 'Paris'"]),
    ("1", "where range", PEOPLE + ["SELECT name FROM people WHERE age >= 30 AND age < 40"]),
    ("1", "not of or", PEOPLE + ["SELECT name FROM people WHERE NOT (city = 'Paris' OR city = 'London')"]),
    ("1", "not equal", PEOPLE + ["SELECT id FROM people WHERE city != 'Paris'", "SELECT id FROM people WHERE city <> 'Paris'"]),
    ("1", "is null", PEOPLE + ["SELECT name FROM people WHERE city IS NULL"]),
    ("1", "is not null", PEOPLE + ["SELECT name FROM people WHERE score IS NOT NULL AND age IS NOT NULL"]),
    ("1", "in list", PEOPLE + ["SELECT name FROM people WHERE city IN ('Paris', 'Berlin')"]),
    ("1", "not in list", PEOPLE + ["SELECT name FROM people WHERE id NOT IN (1, 2, 3)"]),
    ("1", "between", PEOPLE + ["SELECT name FROM people WHERE age BETWEEN 28 AND 33"]),
    ("1", "like ignores case", PEOPLE + ["SELECT name FROM people WHERE name LIKE 'a%'"]),
    ("1", "like underscore", PEOPLE + ["SELECT name FROM people WHERE name LIKE '_r%'"]),
    ("1", "arithmetic on columns", PEOPLE + ["SELECT name, age + 1, age * 2, score / 2 FROM people WHERE id <= 3"]),
    ("1", "integer division and precedence", ["SELECT 7 / 2, 7 % 3, 7.0 / 2, 2 + 3 * 4, (2 + 3) * 4, 10 - 4 - 3, 2 * 3 % 4"]),
    ("1", "concatenation", PEOPLE + ["SELECT name || ' from ' || city FROM people WHERE id IN (1, 4)"]),
    ("1", "concatenating numbers", ["SELECT 'n' || 1, 1 || 2"]),
    ("1", "select without from", ["SELECT 1, 'two', 3.5, NULL"]),
    ("1", "null arithmetic", ["SELECT NULL + 1, 1 - NULL, NULL || 'x'"]),
    ("1", "null comparison", ["SELECT 1 = NULL, NULL = NULL, NULL IS NULL, 1 IS NOT NULL"]),
    ("1", "three-valued logic", ["SELECT NULL AND 0, NULL AND 1, NULL OR 1, NULL OR 0, NOT NULL"]),
    ("1", "in with null", ["SELECT 2 IN (1, 2, NULL), 4 IN (1, 2, NULL), 4 NOT IN (1, 2, NULL), NULL IN (1)"]),
    ("1", "comparisons give integers", ["SELECT 2 > 1, 2 < 1, 'a' < 'b', 'B' < 'a'"]),
    ("1", "equality is case sensitive", ["SELECT 'abc' = 'ABC', 'abc' LIKE 'ABC'"]),
    ("1", "alias", PEOPLE + ["SELECT age AS years FROM people WHERE id = 1"]),
    ("1", "lowercase keywords", PEOPLE + ["select name from people where id = 2"]),
    ("1", "names ignore case", PEOPLE + ["SELECT NAME FROM PEOPLE WHERE ID = 1"]),
    ("1", "quote in string, missing columns", PEOPLE + ["INSERT INTO people (id, name) VALUES (8, 'O''Brien')", "SELECT name, age, city FROM people WHERE id = 8"]),
    ("1", "insert with column order", ["CREATE TABLE t (a INTEGER, b TEXT)", "INSERT INTO t (b, a) VALUES ('x', 1), ('y', 2)", "SELECT a, b FROM t"]),
    ("1", "semicolon and newlines", PEOPLE + ["SELECT name\nFROM people\nWHERE id = 3;"]),
    ("1", "real comparison", PEOPLE + ["SELECT name FROM people WHERE score > 80"]),
    ("1", "where on expressions", PEOPLE + ["SELECT name FROM people WHERE age * 2 > 60 AND city LIKE '%n%'"]),
    ("1", "unary minus", PEOPLE + ["SELECT -age, -(1 + 2) FROM people WHERE id = 2"]),
    ("1", "no rows", PEOPLE + ["SELECT name FROM people WHERE age > 100"]),
    ("1", "empty table", ["CREATE TABLE e (x INTEGER)", "SELECT * FROM e"]),
    ("1", "unknown table", ["!SELECT * FROM nothing"]),
    ("1", "unknown column", PEOPLE + ["!SELECT nope FROM people"]),
    ("1", "syntax error", PEOPLE + ["!SELEC name FROM people"]),
    ("1", "wrong value count", PEOPLE + ["!INSERT INTO people VALUES (9, 'X')"]),
    ("1", "table exists", PEOPLE + ["!CREATE TABLE people (id INTEGER)"]),
    # Turn 2: ordering, limits, distinct, aggregates and groups.
    ("2", "order by", PEOPLE + ["SELECT name FROM people WHERE age IS NOT NULL ORDER BY age"]),
    ("2", "order by desc", PEOPLE + ["SELECT name, score FROM people WHERE score IS NOT NULL ORDER BY score DESC"]),
    ("2", "order by two columns", PEOPLE + ["SELECT city, name FROM people WHERE city IS NOT NULL ORDER BY city DESC, name ASC"]),
    ("2", "order by expression", PEOPLE + ["SELECT name FROM people WHERE age IS NOT NULL ORDER BY age % 10, id"]),
    ("2", "order by alias", PEOPLE + ["SELECT name, age * 2 AS doubled FROM people WHERE age IS NOT NULL ORDER BY doubled DESC"]),
    ("2", "order by unselected column", PEOPLE + ["SELECT name FROM people WHERE score IS NOT NULL ORDER BY score"]),
    ("2", "limit", PEOPLE + ["SELECT name FROM people ORDER BY id LIMIT 3"]),
    ("2", "limit offset", PEOPLE + ["SELECT name FROM people ORDER BY id LIMIT 2 OFFSET 3"]),
    ("2", "limit past the end", PEOPLE + ["SELECT name FROM people ORDER BY id LIMIT 100 OFFSET 5"]),
    ("2", "limit zero", PEOPLE + ["SELECT name FROM people ORDER BY id LIMIT 0"]),
    ("2", "distinct", PEOPLE + ["SELECT DISTINCT city FROM people"]),
    ("2", "distinct pairs", SALES + ["SELECT DISTINCT region, product FROM sales"]),
    ("2", "distinct ordered", PEOPLE + ["SELECT DISTINCT city FROM people WHERE city IS NOT NULL ORDER BY city DESC"]),
    ("2", "count star", PEOPLE + ["SELECT COUNT(*) FROM people"]),
    ("2", "count skips nulls", PEOPLE + ["SELECT COUNT(age), COUNT(city), COUNT(score) FROM people"]),
    ("2", "integer aggregates", PEOPLE + ["SELECT SUM(age), AVG(age), MIN(age), MAX(age) FROM people"]),
    ("2", "real aggregates", PEOPLE + ["SELECT SUM(score), MIN(score), MAX(score) FROM people"]),
    ("2", "text min and max", PEOPLE + ["SELECT MIN(name), MAX(name), MIN(city) FROM people"]),
    ("2", "aggregate with where", SALES + ["SELECT COUNT(*), SUM(qty) FROM sales WHERE region = 'north'"]),
    ("2", "aggregates of no rows", SALES + ["SELECT COUNT(*), SUM(qty), MAX(price) FROM sales WHERE qty > 100"]),
    ("2", "aggregate expression", PEOPLE + ["SELECT MAX(age) - MIN(age), COUNT(*) * 2 FROM people"]),
    ("2", "group by", SALES + ["SELECT region, COUNT(*), SUM(qty) FROM sales GROUP BY region"]),
    ("2", "sum of products by group", SALES + ["SELECT region, SUM(qty * price) FROM sales GROUP BY region ORDER BY region"]),
    ("2", "group by two columns", SALES + ["SELECT region, product, SUM(qty) FROM sales GROUP BY region, product"]),
    ("2", "group by expression", SALES + ["SELECT qty > 5, COUNT(*) FROM sales GROUP BY qty > 5"]),
    ("2", "having", SALES + ["SELECT region, COUNT(*) FROM sales GROUP BY region HAVING COUNT(*) > 1"]),
    ("2", "having on unselected aggregate", SALES + ["SELECT region FROM sales GROUP BY region HAVING SUM(qty) >= 10"]),
    ("2", "order by aggregate alias", SALES + ["SELECT product, SUM(qty) AS total FROM sales GROUP BY product ORDER BY total DESC"]),
    ("2", "average by group", SALES + ["SELECT product, AVG(price) FROM sales GROUP BY product ORDER BY product"]),
    ("2", "where, group, having, order, limit", SALES + ["SELECT region, SUM(qty) AS q FROM sales WHERE price < 2 GROUP BY region HAVING SUM(qty) > 5 ORDER BY q DESC LIMIT 2"]),
    ("2", "aggregate in where", SALES + ["!SELECT region FROM sales WHERE SUM(qty) > 3"]),
    # Turn 3: changes, joins and subqueries.
    ("3", "update one row", PEOPLE + ["UPDATE people SET age = 31 WHERE id = 1", "SELECT id, age FROM people ORDER BY id"]),
    ("3", "update with expression", PEOPLE + ["UPDATE people SET score = score + 10 WHERE city = 'Paris'", "SELECT name, score FROM people ORDER BY id"]),
    ("3", "update two columns", PEOPLE + ["UPDATE people SET city = 'Rome', age = age + 1 WHERE age < 30", "SELECT name, city, age FROM people ORDER BY id"]),
    ("3", "update every row", ORDERS + ["UPDATE orders SET amount = amount * 2", "SELECT id, amount FROM orders ORDER BY id"]),
    ("3", "update to null", PEOPLE + ["UPDATE people SET city = NULL WHERE name = 'Bob'", "SELECT name, city FROM people ORDER BY id"]),
    ("3", "update reads old values", ORDERS + ["UPDATE orders SET person_id = amount, amount = person_id WHERE id = 1", "SELECT person_id, amount FROM orders WHERE id = 1"]),
    ("3", "delete some", ORDERS + ["DELETE FROM orders WHERE amount < 10", "SELECT id FROM orders ORDER BY id"]),
    ("3", "delete all", ORDERS + ["DELETE FROM orders", "SELECT COUNT(*) FROM orders"]),
    ("3", "delete keeps unknown", PEOPLE + ["DELETE FROM people WHERE age > 30", "SELECT id FROM people ORDER BY id"]),
    ("3", "inner join", ORDERS + ["SELECT p.name, o.item FROM people p INNER JOIN orders o ON o.person_id = p.id"]),
    ("3", "inner join with where", ORDERS + ["SELECT p.name, o.item, o.amount FROM people AS p INNER JOIN orders AS o ON o.person_id = p.id WHERE o.amount > 10 ORDER BY o.amount"]),
    ("3", "left join", ORDERS + ["SELECT p.name, o.item FROM people p LEFT JOIN orders o ON o.person_id = p.id ORDER BY p.id, o.id"]),
    ("3", "left join finds missing", ORDERS + ["SELECT p.name FROM people p LEFT JOIN orders o ON o.person_id = p.id WHERE o.id IS NULL"]),
    ("3", "left join with groups", ORDERS + ["SELECT p.name, COUNT(o.id), SUM(o.amount) FROM people p LEFT JOIN orders o ON o.person_id = p.id GROUP BY p.id, p.name"]),
    ("3", "join star", ORDERS + ["SELECT * FROM people p INNER JOIN orders o ON o.person_id = p.id WHERE o.item = 'desk'"]),
    ("3", "table names as qualifiers", ORDERS + ["SELECT people.name, orders.item FROM people INNER JOIN orders ON orders.person_id = people.id WHERE orders.amount >= 40"]),
    ("3", "unqualified unique columns", ORDERS + ["SELECT name, item FROM people INNER JOIN orders ON person_id = people.id WHERE amount < 5"]),
    ("3", "ambiguous column", ORDERS + ["!SELECT id FROM people p INNER JOIN orders o ON o.person_id = p.id"]),
    ("3", "three tables", CITIES + ["SELECT p.name, c.country, o.item FROM people p INNER JOIN cities c ON c.name = p.city INNER JOIN orders o ON o.person_id = p.id"]),
    ("3", "self join", PEOPLE + ["SELECT a.name, b.name FROM people a INNER JOIN people b ON a.city = b.city AND a.id < b.id"]),
    ("3", "condition in on", ORDERS + ["SELECT p.name, o.item FROM people p LEFT JOIN orders o ON o.person_id = p.id AND o.amount > 10"]),
    ("3", "in subquery", ORDERS + ["SELECT name FROM people WHERE id IN (SELECT person_id FROM orders WHERE item = 'book')"]),
    ("3", "not in subquery", ORDERS + ["SELECT name FROM people WHERE id NOT IN (SELECT person_id FROM orders)"]),
    ("3", "in grouped subquery", ORDERS + ["SELECT item FROM orders WHERE amount IN (SELECT MAX(amount) FROM orders GROUP BY person_id)"]),
    ("3", "update with subquery", ORDERS + ["UPDATE people SET score = 0 WHERE id IN (SELECT person_id FROM orders WHERE amount > 50)", "SELECT name, score FROM people ORDER BY id"]),
    ("3", "delete with subquery", ORDERS + ["DELETE FROM orders WHERE person_id NOT IN (SELECT id FROM people)", "SELECT id FROM orders ORDER BY id"]),
    ("3", "update unknown column", PEOPLE + ["!UPDATE people SET nope = 1"]),
    ("3", "delete from unknown table", ["!DELETE FROM nothing"]),
    ("case", "case with else", PEOPLE + ["SELECT name, CASE WHEN age < 30 THEN 'young' WHEN age < 36 THEN 'mid' ELSE 'senior' END FROM people"]),
    ("case", "case without else", ORDERS + ["SELECT id, CASE WHEN amount > 50 THEN 'big' END FROM orders"]),
    ("case", "case in aggregate", ORDERS + ["SELECT SUM(CASE WHEN item = 'pen' THEN amount ELSE 0 END) FROM orders"]),
    ("case", "case in where and order", PEOPLE + ["SELECT name FROM people WHERE CASE WHEN city IS NULL THEN 1 ELSE 0 END = 0 ORDER BY CASE WHEN city = 'Paris' THEN 0 ELSE 1 END, id"]),
    # Turn 4: files and transactions.
    ("4", "data survives reopening", ["CREATE TABLE kv (k TEXT, v INTEGER)", "INSERT INTO kv VALUES ('a', 1), ('b', 2)", REOPEN, "SELECT k, v FROM kv ORDER BY k"]),
    ("4", "tables survive reopening", ["CREATE TABLE a (x INTEGER)", "CREATE TABLE b (y TEXT)", "INSERT INTO a VALUES (1), (2)", "INSERT INTO b VALUES ('z')", REOPEN,
                                       "SELECT x FROM a ORDER BY x", "SELECT y FROM b", "INSERT INTO a VALUES (3)", REOPEN, "SELECT COUNT(*) FROM a"]),
    ("4", "changes survive reopening", PEOPLE + ["UPDATE people SET age = 50 WHERE id = 1", "DELETE FROM people WHERE id = 2", REOPEN, "SELECT id, age FROM people ORDER BY id"]),
    ("4", "values round-trip", ["CREATE TABLE t (id INTEGER, s TEXT, r REAL, n TEXT)",
                                "INSERT INTO t VALUES (1, 'it''s', 2.5, NULL), (2, '', -0.125, 'x'), (3, '中文', 1e20, 'NULL')", REOPEN, "SELECT * FROM t ORDER BY id"]),
    ("4", "table still exists", ["CREATE TABLE t (x INTEGER)", REOPEN, "!CREATE TABLE t (x INTEGER)"]),
    ("4", "a thousand rows survive", ["CREATE TABLE n (i INTEGER, s TEXT)", THOUSAND, REOPEN, "SELECT COUNT(*), SUM(i), MAX(s) FROM n"]),
    ("4", "queries after reopening", ORDERS + [REOPEN, "SELECT p.name, COUNT(o.id) FROM people p LEFT JOIN orders o ON o.person_id = p.id GROUP BY p.name ORDER BY p.name"]),
    ("4", "commit", ["CREATE TABLE t (x INTEGER)", "BEGIN", "INSERT INTO t VALUES (1)", "COMMIT", REOPEN, "SELECT x FROM t"]),
    ("4", "rollback", ["CREATE TABLE t (x INTEGER)", "INSERT INTO t VALUES (1)", "BEGIN", "INSERT INTO t VALUES (2)", "UPDATE t SET x = 10 WHERE x = 1",
                       "SELECT x FROM t ORDER BY x", "ROLLBACK", "SELECT x FROM t ORDER BY x", REOPEN, "SELECT x FROM t ORDER BY x"]),
    ("4", "close drops uncommitted changes", ["CREATE TABLE t (x INTEGER)", "INSERT INTO t VALUES (1)", "BEGIN", "INSERT INTO t VALUES (2)", REOPEN, "SELECT x FROM t ORDER BY x"]),
    ("4", "rollback undoes a new table", ["CREATE TABLE keep (x INTEGER)", "BEGIN", "CREATE TABLE tmp (x INTEGER)", "INSERT INTO tmp VALUES (1)", "ROLLBACK", "!SELECT * FROM tmp", REOPEN, "!SELECT * FROM tmp"]),
    ("4", "rollback undoes a delete", PEOPLE + [REOPEN, "BEGIN", "DELETE FROM people", "SELECT COUNT(*) FROM people", "ROLLBACK", "SELECT COUNT(*) FROM people"]),
    ("4", "statements commit on their own", ["CREATE TABLE t (x INTEGER)", "INSERT INTO t VALUES (1)", "UPDATE t SET x = 2", REOPEN, "SELECT x FROM t"]),
    ("4", "begin inside a transaction", ["CREATE TABLE t (x INTEGER)", "BEGIN", "!BEGIN", "INSERT INTO t VALUES (1)", "COMMIT", REOPEN, "SELECT x FROM t"]),
    ("4", "commit and rollback need a transaction", ["CREATE TABLE t (x INTEGER)", "!COMMIT", "!ROLLBACK", "SELECT COUNT(*) FROM t"]),
    ("4", "failed statement keeps the transaction", ["CREATE TABLE t (x INTEGER)", "BEGIN", "INSERT INTO t VALUES (1)", "!INSERT INTO nope VALUES (1)", "COMMIT", REOPEN, "SELECT x FROM t"]),
    # Turn 5: the reported bugs, their neighbours, and indexes.
    ("5", "reported: negative division", ["SELECT -7 / 2, -7 % 3"]),
    ("5", "negative division and remainder", ["SELECT 7 / -2, 7 % -3, -9 / 4, -9 % 4, -1 / 3"]),
    ("5", "reported: division by zero", ["SELECT 1 / 0"]),
    ("5", "division by zero gives null", ["SELECT 5 % 0, 1.5 / 0, 0 / 0, 2.5 % 0"]),
    ("5", "division by zero in where", PEOPLE + ["SELECT name FROM people WHERE age / 0 IS NULL"]),
    ("5", "negative division of a sum", ORDERS + ["SELECT SUM(amount) / -4, SUM(amount) % -7 FROM orders"]),
    ("5", "reported: nulls sort first", PEOPLE + ["SELECT name FROM people ORDER BY score, id"]),
    ("5", "reported: nulls sort last descending", PEOPLE + ["SELECT name FROM people ORDER BY score DESC, id"]),
    ("5", "null text sorts first", PEOPLE + ["SELECT name FROM people ORDER BY city, name"]),
    ("5", "null sorts last descending", PEOPLE + ["SELECT name FROM people ORDER BY age DESC, name"]),
    ("5", "null group sorts first", PEOPLE + ["SELECT city, COUNT(*) FROM people GROUP BY city ORDER BY city"]),
    ("5", "index lookup", PEOPLE + ["CREATE INDEX idx_city ON people (city)", "SELECT name FROM people WHERE city = 'Paris'"]),
    ("5", "index after insert", ORDERS + ["CREATE INDEX idx_person ON orders (person_id)", "INSERT INTO orders VALUES (8, 4, 'cup', 5)", "SELECT item FROM orders WHERE person_id = 4"]),
    ("5", "index after update", PEOPLE + ["CREATE INDEX idx_city ON people (city)", "UPDATE people SET city = 'Paris' WHERE id = 2",
                                          "SELECT name FROM people WHERE city = 'Paris'", "SELECT name FROM people WHERE city = 'London'"]),
    ("5", "index after delete", PEOPLE + ["CREATE INDEX idx_city ON people (city)", "DELETE FROM people WHERE city = 'Paris'",
                                          "SELECT name FROM people WHERE city = 'Paris'", "SELECT COUNT(*) FROM people"]),
    ("5", "index and nulls", PEOPLE + ["CREATE INDEX idx_city ON people (city)", "SELECT name FROM people WHERE city IS NULL", "SELECT name FROM people WHERE city = NULL"]),
    ("5", "index and ranges", PEOPLE + ["CREATE INDEX idx_age ON people (age)", "SELECT id FROM people WHERE age > 30", "SELECT id FROM people WHERE age = 30 OR age = 25"]),
    ("5", "index survives reopening", ["CREATE TABLE t (k INTEGER, v TEXT)", "INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c')", "CREATE INDEX idx_k ON t (k)", REOPEN,
                                       "INSERT INTO t VALUES (4, 'd')", "UPDATE t SET k = 20 WHERE k = 2", "SELECT v FROM t WHERE k = 4", "SELECT v FROM t WHERE k = 20", "SELECT v FROM t WHERE k = 2"]),
    ("5", "index rolls back", ["CREATE TABLE t (k INTEGER, v TEXT)", "CREATE INDEX idx_k ON t (k)", "INSERT INTO t VALUES (1, 'a')", "BEGIN", "INSERT INTO t VALUES (2, 'b')",
                               "DELETE FROM t WHERE k = 1", "ROLLBACK", "SELECT v FROM t WHERE k = 2", "SELECT v FROM t WHERE k = 1"]),
    ("5", "index in a join", ORDERS + ["CREATE INDEX idx_person ON orders (person_id)", "SELECT p.name, o.item FROM people p INNER JOIN orders o ON o.person_id = p.id"]),
    ("5", "index on unknown table", ["!CREATE INDEX idx ON nothing (x)"]),
    ("5", "index on unknown column", PEOPLE + ["!CREATE INDEX idx ON people (nope)"]),
    ("5", "index name taken", PEOPLE + ["CREATE INDEX idx ON people (city)", "!CREATE INDEX idx ON people (age)"]),
]

ROWS = 100_000
SPEED_CASE = ("5", "point queries use an index")


def is_select(sql):
    return sql.lstrip().upper().startswith("SELECT")


def ordered(sql):
    return re.search(r"\bORDER\s+BY\b", sql, re.I) is not None


def same(want, got):
    """Equal in SQLite's terms: same type, floats within 1e-9."""
    if want is None or got is None:
        return want is None and got is None
    if isinstance(want, int):
        return type(got) is int and want == got
    if isinstance(want, float):
        return type(got) is float and math.isclose(want, got, rel_tol=1e-9, abs_tol=1e-12)
    return type(got) is type(want) and want == got


def sort_key(row):
    def key(value):
        if value is None:
            return (0, 0)
        if isinstance(value, (int, float)):
            return (1, float(value))
        if isinstance(value, str):
            return (2, value)
        return (3, repr(value))
    return [key(value) for value in row]


def rows_agree(want, got, in_order):
    if len(want) != len(got):
        return False
    if not in_order:
        want, got = sorted(want, key=sort_key), sorted(got, key=sort_key)
    return all(len(w) == len(g) and all(same(a, b) for a, b in zip(w, g)) for w, g in zip(want, got))


def agree(sql, want, got):
    if want[0] == "rows":
        return got[0] == "rows" and rows_agree(want[1], got[1], ordered(sql))
    return got[0] == want[0]


def show(outcome):
    if outcome[0] == "rows":
        rows = outcome[1]
        return repr(rows[:6]) + (f" … {len(rows)} rows" if len(rows) > 6 else "")
    return " ".join(str(part) for part in outcome)


class Oracle:
    def __init__(self, path):
        import sqlite3
        self.sqlite3, self.path = sqlite3, path
        self.conn = sqlite3.connect(path, isolation_level=None)

    def run(self, sql):
        try:
            rows = self.conn.execute(sql).fetchall()
        except self.sqlite3.Error as error:
            return ("error", str(error))
        return ("rows", rows) if is_select(sql) else ("ok",)

    def reopen(self):
        self.conn.close()
        self.conn = self.sqlite3.connect(self.path, isolation_level=None)

    def close(self):
        self.conn.close()


class Engine:
    """minisql as the agent wrote it; every workspace module it loads is
    remembered for the sqlite3 check."""

    def __init__(self, root, path, loaded):
        self.root, self.path, self.loaded = root, path, loaded
        self.conn = None
        self.module = self.load()
        self.conn = self.module.connect(path)

    def load(self):
        module = importlib.import_module("minisql")
        self.error = getattr(module, "Error", None)
        if not (isinstance(self.error, type) and issubclass(self.error, BaseException)):
            self.error = ()
        return module

    def workspace_modules(self):
        yield from workspace_modules(self.root)

    def run(self, sql):
        try:
            rows = self.conn.execute(sql)
            if is_select(sql):
                rows = [tuple(row) for row in rows]
        except self.error as error:
            return ("error", str(error))
        except Exception as error:
            return ("crash", f"{type(error).__name__}: {error}")
        return ("rows", rows) if is_select(sql) else ("ok",)

    def reopen(self):
        """Close, forget every module loaded from the workspace, connect again."""
        try:
            self.conn.close()
        except Exception as error:
            return ("crash", f"close: {type(error).__name__}: {error}")
        self.conn = None
        for name, path in list(self.workspace_modules()):
            self.loaded.add(path)
            del sys.modules[name]
        importlib.invalidate_caches()
        try:
            self.module = self.load()
            self.conn = self.module.connect(self.path)
        except Exception as error:
            return ("crash", f"reconnect: {type(error).__name__}: {error}")
        return None

    def close(self):
        self.loaded.update(path for _, path in self.workspace_modules())
        try:
            if self.conn is not None:
                self.conn.close()
        except Exception:
            pass


def run_case(root, steps, loaded):
    """None when minisql agrees with SQLite throughout, else where it first differs."""
    work = tempfile.mkdtemp(prefix="minisql-case-")
    on_disk = REOPEN in steps
    oracle = engine = None
    try:
        oracle = Oracle(os.path.join(work, "oracle.db") if on_disk else ":memory:")
        try:
            engine = Engine(root, os.path.join(work, "engine.db") if on_disk else ":memory:", loaded)
        except Exception as error:
            return {"statement": "connect", "got": f"{type(error).__name__}: {error}"}
        for step in steps:
            if step == REOPEN:
                oracle.reopen()
                failed = engine.reopen()
                if failed:
                    return {"statement": REOPEN, "got": show(failed)}
                continue
            sql = step[1:] if step.startswith("!") else step
            want = oracle.run(sql)
            if (want[0] == "error") != step.startswith("!"):
                return {"broken": True, "statement": step, "want": show(want)}
            got = engine.run(sql)
            if not agree(sql, want, got):
                return {"statement": sql, "want": show(want), "got": show(got)}
        return None
    finally:
        for connection in (engine, oracle):
            if connection is not None:
                connection.close()
        shutil.rmtree(work, ignore_errors=True)


def index_speed(root, loaded):
    """Point queries on an indexed column of 100,000 rows are fast: under
    20 ms each, or ten times faster than a scan of an unindexed column."""
    engine = Engine(root, ":memory:", loaded)
    try:
        def must(sql, want):
            got = engine.run(sql)
            if got != want:
                raise AssertionError({"statement": sql[:80], "want": show(want), "got": show(got)})
        must("CREATE TABLE big (id INTEGER, name TEXT, v INTEGER)", ("ok",))
        started = time.perf_counter()
        for first in range(0, ROWS, 1000):
            must("INSERT INTO big VALUES " + ", ".join(f"({i}, 'n{i}', {i % 1000})" for i in range(first, first + 1000)), ("ok",))
            if time.perf_counter() - started > 300:
                return {"statement": "INSERT", "got": f"inserting {ROWS} rows took over 300 s"}
        must("CREATE INDEX big_id ON big (id)", ("ok",))
        scans = []
        for _ in range(3):
            began = time.perf_counter()
            must("SELECT name FROM big WHERE v = 1000", ("rows", []))
            scans.append(time.perf_counter() - began)
        points, chosen = [], random.Random(7)
        deadline = time.perf_counter() + 30
        for _ in range(200):
            key = chosen.randrange(ROWS)
            began = time.perf_counter()
            must(f"SELECT name FROM big WHERE id = {key}", ("rows", [(f"n{key}",)]))
            points.append(time.perf_counter() - began)
            if time.perf_counter() > deadline:
                break
        point, scan = sum(points) / len(points), sum(scans) / len(scans)
        if point < 0.02 or point * 10 < scan:
            return None
        return {"statement": "SELECT name FROM big WHERE id = …", "got": f"{point * 1000:.1f} ms per indexed query, {scan * 1000:.1f} ms per scan"}
    except AssertionError as failure:
        return failure.args[0]
    finally:
        engine.close()


def workspace_modules(root):
    """Loaded modules whose files are in the workspace; this script is not one."""
    for name, module in list(sys.modules.items()):
        path = getattr(module, "__file__", None)
        if name != "__main__" and path and os.path.abspath(path).startswith(os.path.abspath(root) + os.sep):
            yield name, os.path.abspath(path)


IMPORTS_SQLITE = re.compile(r"^\s*(?:import\s+(?:[\w.]+\s*,\s*)*_?sqlite3\b|from\s+_?sqlite3\b)|import_module\(\s*['\"]_?sqlite3|__import__\(\s*['\"]_?sqlite3", re.M)


def main():
    root, turn = os.path.abspath(sys.argv[1]), int(sys.argv[2])
    sys.path.insert(0, root)
    groups = [group for group in TURN if TURN[group] <= turn]
    score = {"turn": turn, "groups": {group: {"passed": 0, "total": 0} for group in groups}, "failures": [], "broken": []}
    try:
        importlib.import_module("minisql")
    except BaseException as error:
        score["import_error"] = f"{type(error).__name__}: {error}"
    loaded = set()
    cases = [case for case in CASES if TURN[case[0]] <= turn]
    for group, name, steps in cases + ([(*SPEED_CASE, None)] if turn >= 5 else []):
        try:
            if "import_error" in score:
                failure = {"got": score["import_error"]}
            elif steps is None:
                failure = index_speed(root, loaded)
            else:
                failure = run_case(root, steps, loaded)
        except BaseException as error:  # the case's crash, not the run's
            failure = {"got": f"{type(error).__name__}: {error}"}
        if failure and failure.pop("broken", False):
            score["broken"].append({"case": name, **failure})
            continue
        score["groups"][group]["total"] += 1
        if failure:
            score["failures"].append({"group": group, "case": name, **failure})
        else:
            score["groups"][group]["passed"] += 1
    loaded.update(path for _, path in workspace_modules(root))
    score["uses_sqlite3"] = any(IMPORTS_SQLITE.search(open(path, encoding="utf-8", errors="replace").read())
                                for path in sorted(loaded) if path.endswith(".py") and os.path.isfile(path))
    score["passed"] = sum(group["passed"] for group in score["groups"].values())
    score["total"] = sum(group["total"] for group in score["groups"].values())
    print(json.dumps(score, ensure_ascii=False))


if __name__ == "__main__":
    main()
