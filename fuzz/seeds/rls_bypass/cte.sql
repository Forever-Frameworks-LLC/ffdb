WITH visible AS (SELECT * FROM documents) SELECT count(*), max(body) FROM visible;
