SELECT count(*), min(body), length(group_concat(body)) FROM documents;
