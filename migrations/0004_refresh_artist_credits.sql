-- Group snapshots written before structured artist credits were added
-- deserialize with an empty credits array. Force a tracker-authoritative
-- refresh without disturbing canonical releases or durable library links.
DELETE FROM tracker_snapshots WHERE resource_kind = 'group';
