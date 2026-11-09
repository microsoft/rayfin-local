//! System tables are component-owned, never part of app entity migration snapshots.

pub(crate) const STEPS: &[&str] = &["CREATE TABLE Users (
        Id TEXT PRIMARY KEY NOT NULL COLLATE NOCASE
            CHECK (length(Id) = 36 AND substr(Id, 9, 1) = '-'
                AND substr(Id, 14, 1) = '-' AND substr(Id, 19, 1) = '-'
                AND substr(Id, 24, 1) = '-'
                AND length(replace(Id, '-', '')) = 32
                AND replace(Id, '-', '') NOT GLOB '*[^0-9a-fA-F]*'),
        Email TEXT NOT NULL CHECK (length(Email) <= 255)
    ) STRICT;"];
