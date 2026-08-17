# -*- coding: utf-8 -*-
import signal
import discord
from discord.ext import commands, tasks
import discord.utils
import asyncio
import copy
import os
import logging
import sys

# Force UTF-8 output on all platforms (fixes garbled emoji/symbols on Windows)
try:
    if hasattr(sys.stdout, 'reconfigure'):
        sys.stdout.reconfigure(encoding='utf-8')
    if hasattr(sys.stderr, 'reconfigure'):
        sys.stderr.reconfigure(encoding='utf-8')
except Exception:
    pass
from datetime import datetime, timedelta, timezone
import random
import json
from discord.ui import Button, View, Modal, TextInput, Select
from discord import app_commands
import time
from logging.handlers import RotatingFileHandler
from typing import Optional, Set, Dict, Tuple, List, Any, Callable
import threading
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path
import sqlite3
import pickle
import re
import uuid as _uuid
import html as _html
from urllib.parse import urlparse


def _esc(value) -> str:
    """HTML-escape user-controlled text for safe embedding in transcript HTML."""
    if value is None:
        return ""
    return _html.escape(str(value), quote=True)


def _safe_url(url) -> str:
    """Return the URL only if it parses to a safe http(s) scheme, else empty."""
    if not url:
        return ""
    try:
        parsed = urlparse(str(url))
    except Exception:
        return ""
    if parsed.scheme in ("http", "https") and parsed.netloc:
        return _esc(str(url))
    return ""

try:
    import aiosqlite
    SQLITE_AVAILABLE = True
except ImportError:
    SQLITE_AVAILABLE = False

try:
    from flask import Flask
    FLASK_AVAILABLE = True
except ImportError:
    FLASK_AVAILABLE = False

# Roblox in-game verification support removed; server-only verification flow is used.


# --- ENUMERATIONS ---
class VerificationStatus(Enum):
    PENDING = "pending"
    APPROVED = "approved"
    DENIED = "denied"
    PENDING_INFO = "pending_info"


class TicketStatus(Enum):
    OPEN = "open"
    IN_PROGRESS = "in_progress"
    RESOLVED = "resolved"
    CLOSED = "closed"


class GiveawayStatus(Enum):
    ACTIVE = "active"
    ENDED = "ended"
    CANCELLED = "cancelled"


class WarningType(Enum):
    SPAM = "spam"
    HARASSMENT = "harassment"
    TOXIC = "toxic"
    RAID = "raid"
    ADVERTISING = "advertising"
    NSFW = "nsfw"
    CUSTOM = "custom"


# --- CONFIGURATION ---
@dataclass
class ChannelConfig:
    invite: int = 1471663457287012497
    welcome: int = 1471663455017767107
    rules: int = 1471663457287012497
    log: int = 1529562857602289776
    auto_scan: int = 1471663527247741001
    verification_main: int = 1471663452677476520
    verification_submission: int = 1471663454115860611
    reports: int = 1329157448564609056
    tickets: int = 1471663451561525522 # TICKET CATEGORY (DO NOT CHANGE)
    transcripts: int = 1471663530238546093 # TRANSCRIPTS CHANNEL
    giveaways: int = 1471663463599309066


@dataclass
class RoleConfig:
    invite_manager: int = 1471663385568350282
    member: int = 1471663405877166090
    staff: int = 1471663388848423087
    verified: int = 1411440990408937642
    verification_ping: int = 1476162221632389220
    muted: int = 1471663391067078777
    ticket_support: int = 1476162221632389220
    server_tester: int = 1479065086420193392  # Set this to your Server Tester role ID


@dataclass
class ServerConfig:
    gang_server_id: int = 1471662897397633034
    gang_rules_channel: int = 1471663457287012497
    gang_rules_message: int = 1473423973797593198
    server_server_id: int = 1163937669068357844
    server_rules_channel: int = 1163938532964978718
    server_rules_message: int = 1453140236732469258


@dataclass
class TimingConfig:
    invite_check_interval_minutes: int = 5
    auto_scan_interval_hours: int = 2
    report_message_interval_minutes: int = 30
    verification_timeout_seconds: int = 180
    confirmation_timeout_seconds: int = 60
    report_timeout_seconds: int = 300
    info_request_timeout_seconds: int = 300


@dataclass
class LimitsConfig:
    min_account_age_days: int = 6
    min_blacklist_keyword_length: int = 2
    min_poll_options: int = 2
    max_poll_options: int = 10
    min_poll_duration: int = 30
    max_poll_duration: int = 86400
    max_warnings_before_ban: int = 3
    max_tickets_per_user: int = 3
    max_giveaway_winners: int = 10


class Config:
    def __init__(self):
        self.channels = ChannelConfig()
        self.roles = RoleConfig()
        self.servers = ServerConfig()
        self.timing = TimingConfig()
        self.limits = LimitsConfig()

        self.data_dir = "data"
        self.json_dir = "data/JsonData"
        self.db_file = "data/bot_data.db"
        self.lock_file = "bot_busy.lock"

        self.tickets_data_file = "data/JsonData/tickets_data.json"
        self.giveaways_data_file = "data/JsonData/giveaways_data.json"
        self.branding_file = "data/JsonData/branding.json"
        self.channel_config_file = "data/JsonData/channel_config.json"

        self.enable_leveling = True
        self.enable_tickets = True
        self.enable_giveaways = True
        self.enable_warnings = True

        self.command_prefix = "!"
        self.bot_status = "[GANG NAME] On Top"
        self.debug_mode = False
        self.gang_name = "[GANG NAME]"
        self.gang_abbreviation = "GANG"

    def ensure_directories(self) -> None:
        os.makedirs(self.data_dir, exist_ok=True)
        os.makedirs(self.json_dir, exist_ok=True)

    def load_branding_settings(self) -> None:
        try:
            payload = data_manager.get_config_value("branding")
            if not payload:
                return
            data = json.loads(payload)
            if isinstance(data, dict):
                self.gang_name = str(data.get("gang_name") or self.gang_name)
                self.gang_abbreviation = str(data.get("gang_abbreviation") or self.gang_abbreviation).upper()
                self.bot_status = str(data.get("bot_status") or self.bot_status)
        except Exception as exc:
            logging.warning(f"[Branding] Could not load branding config: {exc}")

    def save_branding_settings(self) -> None:
        try:
            payload = json.dumps({
                "gang_name": self.gang_name,
                "gang_abbreviation": self.gang_abbreviation,
                "bot_status": self.bot_status,
            })
            data_manager.set_config_value("branding", payload)
        except Exception as exc:
            logging.warning(f"[Branding] Could not save branding config: {exc}")

    def load_channel_settings(self) -> None:
        try:
            payload = data_manager.get_config_value("channels")
            if not payload:
                return
            data = json.loads(payload)
            if not isinstance(data, dict):
                return
            for key, value in data.items():
                if not isinstance(value, int):
                    continue
                if hasattr(self.channels, key):
                    setattr(self.channels, key, value)
                elif hasattr(self.servers, key):
                    setattr(self.servers, key, value)
        except Exception as exc:
            logging.warning(f"[Channels] Could not load channel config: {exc}")

    def save_channel_settings(self) -> None:
        try:
            payload = json.dumps({
                "invite": self.channels.invite,
                "welcome": self.channels.welcome,
                "rules": self.channels.rules,
                "log": self.channels.log,
                "auto_scan": self.channels.auto_scan,
                "verification_main": self.channels.verification_main,
                "verification_submission": self.channels.verification_submission,
                "reports": self.channels.reports,
                "tickets": self.channels.tickets,
                "transcripts": self.channels.transcripts,
                "giveaways": self.channels.giveaways,
                "gang_rules_channel": self.servers.gang_rules_channel,
                "server_rules_channel": self.servers.server_rules_channel,
            })
            data_manager.set_config_value("channels", payload)
        except Exception as exc:
            logging.warning(f"[Channels] Could not save channel config: {exc}")


# --- SQLITE DATA MANAGER (For persistent data - saves PC resources) ---
class DataManager:
    """
    SQLite-based data manager for persistent data.
    More efficient than JSON for frequent read/write operations.
    Stores: invites, blacklist, rules_cache, levels, warnings
    """

    def __init__(self, db_path: str):
        self.db_path = db_path
        self._connection: Optional[sqlite3.Connection] = None
        self._lock = threading.Lock()

    def connect(self) -> None:
        # Guard against the on_ready reconnect case: discord.py can fire
        # on_ready again after a gateway reconnect, and without this guard a
        # second sqlite3.Connection would be opened (leaking the first one and
        # leaving _create_tables() to run again). If we're already connected,
        # the tables already exist — nothing to do.
        if self._connection is not None:
            logging.info("[DataManager] SQLite connection already established; reusing it.")
            return
        self._connection = sqlite3.connect(self.db_path, check_same_thread=False)
        self._connection.row_factory = sqlite3.Row
        self._create_tables()
        logging.info(f"[DataManager] Connected to database: {self.db_path}")

    def _create_tables(self) -> None:
        cursor = self._connection.cursor()

        cursor.execute('''
            CREATE TABLE IF NOT EXISTS invites (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                message_id INTEGER,
                channel_id INTEGER,
                tracked_invites BLOB
            )
        ''')

        # Archive of previous invite batches, so the "View Previous Invites"
        # button can show a historical record after a regenerate/expire cycle.
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS invite_history (
                history_id TEXT PRIMARY KEY,
                archived_at TEXT NOT NULL,
                archived_by INTEGER,
                invite_code TEXT NOT NULL,
                invite_name TEXT,
                max_uses INTEGER DEFAULT 0,
                final_uses INTEGER DEFAULT 0,
                status TEXT DEFAULT 'expired',
                created_at TEXT,
                guild_id INTEGER
            )
        ''')
        # Migration: older databases may not have invite_history yet — the
        # CREATE TABLE IF NOT EXISTS above already handles that, so nothing
        # else to do here.

        # Tracks every active !getallroles embed so we can auto-update them
        # when roles are created/deleted/renamed, and re-attach the persistent
        # "Copy List" button after a bot restart.
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS getallroles_messages (
                message_id INTEGER PRIMARY KEY,
                channel_id INTEGER NOT NULL,
                guild_id INTEGER NOT NULL,
                created_by INTEGER,
                created_at TEXT NOT NULL
            )
        ''')

        cursor.execute('''
            CREATE TABLE IF NOT EXISTS blacklist (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                keywords BLOB
            )
        ''')

        cursor.execute('''
            CREATE TABLE IF NOT EXISTS rules_cache (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                gang_rules TEXT,
                server_rules TEXT,
                gang_last_updated TEXT,
                server_last_updated TEXT
            )
        ''')

        # Migration: older databases may lack gang_rules / server_rules columns
        cursor.execute("PRAGMA table_info(rules_cache)")
        existing_cols = {row[1] for row in cursor.fetchall()}
        if 'gang_rules' not in existing_cols:
            cursor.execute('ALTER TABLE rules_cache ADD COLUMN gang_rules TEXT')
        if 'server_rules' not in existing_cols:
            cursor.execute('ALTER TABLE rules_cache ADD COLUMN server_rules TEXT')
        if 'gang_last_updated' not in existing_cols:
            cursor.execute('ALTER TABLE rules_cache ADD COLUMN gang_last_updated TEXT')
        if 'server_last_updated' not in existing_cols:
            cursor.execute('ALTER TABLE rules_cache ADD COLUMN server_last_updated TEXT')

        cursor.execute('''
            CREATE TABLE IF NOT EXISTS levels (
                user_id INTEGER,
                guild_id INTEGER,
                xp INTEGER DEFAULT 0,
                level INTEGER DEFAULT 0,
                total_messages INTEGER DEFAULT 0,
                last_xp_gain TEXT,
                PRIMARY KEY (user_id, guild_id)
            )
        ''')

        cursor.execute('''
            CREATE TABLE IF NOT EXISTS warnings (
                warning_id TEXT PRIMARY KEY,
                user_id INTEGER,
                guild_id INTEGER,
                moderator_id INTEGER,
                warning_type TEXT,
                reason TEXT,
                points INTEGER DEFAULT 1,
                created_at TEXT,
                expires_at TEXT,
                is_active INTEGER DEFAULT 1
            )
        ''')

        # =============================================================================
        # TICKET TOOL TABLES (Full Ticket Tool Clone)
        # =============================================================================

        # Ticket Panels - Store panel configurations
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS ticket_panels (
                panel_id TEXT PRIMARY KEY,
                guild_id INTEGER,
                channel_id INTEGER,
                message_id INTEGER,
                name TEXT,
                description TEXT,
                embed_title TEXT,
                embed_description TEXT,
                embed_color INTEGER,
                embed_image TEXT,
                embed_thumbnail TEXT,
                button_style INTEGER,
                button_label TEXT,
                button_emoji TEXT,
                category_id INTEGER,
                support_role_id INTEGER,
                ticket_limit INTEGER DEFAULT 1,
                auto_close_hours INTEGER DEFAULT 24,
                welcome_message TEXT,
                claim_required INTEGER DEFAULT 0,
                created_at TEXT,
                is_active INTEGER DEFAULT 1
            )
        ''')

        # Tickets - Store individual ticket data
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS tickets (
                ticket_id TEXT PRIMARY KEY,
                guild_id INTEGER,
                channel_id INTEGER,
                panel_id TEXT,
                creator_id INTEGER,
                category TEXT,
                subject TEXT,
                claimed_by INTEGER,
                claimed_at TEXT,
                status TEXT DEFAULT 'open',
                created_at TEXT,
                closed_at TEXT,
                closed_by INTEGER,
                close_reason TEXT,
                rating INTEGER,
                rating_feedback TEXT
            )
        ''')

        # Ticket Transcripts - Store transcript data
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS ticket_transcripts (
                transcript_id TEXT PRIMARY KEY,
                ticket_id TEXT,
                guild_id INTEGER,
                channel_id INTEGER,
                creator_id INTEGER,
                closed_by INTEGER,
                claimed_by INTEGER,
                category TEXT,
                created_at TEXT,
                closed_at TEXT,
                message_count INTEGER,
                file_path TEXT,
                html_content TEXT
            )
        ''')

        # Ticket Blacklist - Users blocked from creating tickets
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS ticket_blacklist (
                blacklist_id TEXT PRIMARY KEY,
                guild_id INTEGER,
                user_id INTEGER,
                reason TEXT,
                blacklisted_by INTEGER,
                blacklisted_at TEXT,
                expires_at TEXT,
                is_active INTEGER DEFAULT 1
            )
        ''')

        # Ticket Settings - Per-guild ticket settings
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS ticket_settings (
                guild_id INTEGER PRIMARY KEY,
                category_id INTEGER,
                transcripts_channel_id INTEGER,
                log_channel_id INTEGER,
                support_role_id INTEGER,
                admin_role_id INTEGER,
                max_tickets_per_user INTEGER DEFAULT 3,
                auto_close_hours INTEGER DEFAULT 24,
                mention_on_create INTEGER DEFAULT 1,
                dm_transcripts INTEGER DEFAULT 1,
                require_claim INTEGER DEFAULT 0,
                created_at TEXT,
                updated_at TEXT
            )
        ''')

        # Ticket Questions - Custom questions for panels
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS ticket_questions (
                question_id TEXT PRIMARY KEY,
                panel_id TEXT,
                guild_id INTEGER,
                question_text TEXT,
                question_type TEXT DEFAULT 'text',
                required INTEGER DEFAULT 1,
                placeholder TEXT,
                order_index INTEGER,
                created_at TEXT
            )
        ''')

        # Ticket Answers - Store user answers to questions
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS ticket_answers (
                answer_id TEXT PRIMARY KEY,
                ticket_id TEXT,
                question_id TEXT,
                user_id INTEGER,
                answer_text TEXT,
                answered_at TEXT
            )
        ''')

        # Ticket Messages - Track messages for transcripts
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS ticket_messages (
                message_id INTEGER PRIMARY KEY,
                ticket_id TEXT,
                author_id INTEGER,
                author_name TEXT,
                author_avatar TEXT,
                content TEXT,
                attachments TEXT,
                created_at TEXT
            )
        ''')

        # Ticket Notes - Private staff-only notes per ticket
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS ticket_notes (
                note_id TEXT PRIMARY KEY,
                ticket_id TEXT,
                guild_id INTEGER,
                author_id INTEGER,
                content TEXT,
                created_at TEXT
            )
        ''')

        # === MIGRATIONS: add new columns to existing tables safely ===
        migrations = [
            'ALTER TABLE tickets ADD COLUMN priority TEXT DEFAULT "normal"',
            'ALTER TABLE tickets ADD COLUMN first_response_at TEXT',
            'ALTER TABLE ticket_settings ADD COLUMN sla_hours INTEGER DEFAULT 0',
            'ALTER TABLE ticket_panels ADD COLUMN sla_hours INTEGER DEFAULT 0',
        ]
        for sql in migrations:
            try:
                cursor.execute(sql)
            except Exception:
                pass  # Column already exists - safe to ignore

        # BOT CONFIG (Branding & Channel settings - replaces JSON files)
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS bot_config (
                key TEXT PRIMARY KEY,
                value TEXT
            )
        ''')

        # GIVEAWAYS (Persistent giveaways - replaces giveaways_data.json)
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS giveaways (
                giveaway_id TEXT PRIMARY KEY,
                message_id INTEGER,
                channel_id INTEGER,
                guild_id INTEGER,
                host_id INTEGER,
                prize TEXT,
                winner_count INTEGER,
                entries TEXT,
                winners TEXT,
                status TEXT,
                created_at TEXT,
                ends_at TEXT
            )
        ''')

        # === TEMP MUTES (persist across restarts) ===
        # Stores active temp-mutes so they survive a bot restart. A background
        # task polls for expired rows and unmutes the users; this is the source
        # of truth (NOT an in-memory asyncio.sleep), so mutes can never get
        # "stuck forever" if the bot restarts.
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS temp_mutes (
                mute_id TEXT PRIMARY KEY,
                guild_id INTEGER,
                user_id INTEGER,
                role_id INTEGER,
                moderator_id INTEGER,
                reason TEXT,
                muted_at TEXT,
                unmute_at TEXT,
                is_active INTEGER DEFAULT 1
            )
        ''')
        cursor.execute(
            'CREATE INDEX IF NOT EXISTS idx_temp_mutes_unmute ON temp_mutes (unmute_at)'
        )
        cursor.execute(
            'CREATE INDEX IF NOT EXISTS idx_temp_mutes_active ON temp_mutes (is_active)'
        )

        # =============================================================================
        # REACTION ROLES (Carl-bot style, up to 250 mappings per guild)
        # =============================================================================
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS reaction_roles (
                mapping_id TEXT PRIMARY KEY,
                guild_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,
                message_id INTEGER NOT NULL,
                emoji TEXT NOT NULL,
                role_id INTEGER NOT NULL,
                mode TEXT DEFAULT 'normal',
                description TEXT,
                created_by INTEGER,
                created_at TEXT,
                is_active INTEGER DEFAULT 1
            )
        ''')
        cursor.execute(
            'CREATE INDEX IF NOT EXISTS idx_rr_guild ON reaction_roles (guild_id)'
        )
        cursor.execute(
            'CREATE INDEX IF NOT EXISTS idx_rr_msg ON reaction_roles (message_id)'
        )
        cursor.execute(
            'CREATE INDEX IF NOT EXISTS idx_rr_lookup ON reaction_roles (message_id, emoji)'
        )

        # =============================================================================
        # STICKY ROLES (Dyno premium style — re-apply roles on rejoin)
        # =============================================================================
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS sticky_roles (
                entry_id TEXT PRIMARY KEY,
                guild_id INTEGER NOT NULL,
                user_id INTEGER NOT NULL,
                role_ids TEXT NOT NULL,
                saved_at TEXT,
                UNIQUE (guild_id, user_id)
            )
        ''')
        cursor.execute(
            'CREATE INDEX IF NOT EXISTS idx_sticky_lookup ON sticky_roles (guild_id, user_id)'
        )
        # Per-guild sticky-role config (which roles are eligible to be sticky)
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS sticky_role_config (
                guild_id INTEGER PRIMARY KEY,
                enabled INTEGER DEFAULT 0,
                eligible_role_ids TEXT,
                updated_at TEXT
            )
        ''')

        # =============================================================================
        # FULL MESSAGE LOGGING (Dyno premium style — edits + deletes with content)
        # =============================================================================
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS message_log_config (
                guild_id INTEGER PRIMARY KEY,
                enabled INTEGER DEFAULT 0,
                log_channel_id INTEGER,
                log_edits INTEGER DEFAULT 1,
                log_deletes INTEGER DEFAULT 1,
                ignore_bots INTEGER DEFAULT 1,
                ignore_channels TEXT,
                updated_at TEXT
            )
        ''')
        # Cache of recently-seen message content so we can log the *original*
        # text when a message is edited or deleted (Discord doesn't always give
        # us the before-content on delete).
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS message_log_cache (
                message_id INTEGER PRIMARY KEY,
                guild_id INTEGER,
                channel_id INTEGER,
                author_id INTEGER,
                author_name TEXT,
                content TEXT,
                attachments TEXT,
                created_at TEXT
            )
        ''')
        cursor.execute(
            'CREATE INDEX IF NOT EXISTS idx_mlcache_chan ON message_log_cache (channel_id)'
        )

        # =============================================================================
        # CUSTOM BOT BRANDING (avatar / banner / embed footer override)
        # =============================================================================
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS bot_branding (
                guild_id INTEGER PRIMARY KEY,
                embed_footer TEXT,
                embed_color INTEGER,
                embed_thumbnail TEXT,
                embed_image TEXT,
                avatar_url TEXT,
                banner_url TEXT,
                updated_at TEXT
            )
        ''')

        # =====================================================================
        # OWS PERSISTENT PANEL (remembers the open !ows menu on restart)
        # =====================================================================
        cursor.execute('''
            CREATE TABLE IF NOT EXISTS ows_active_panel (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                message_id INTEGER NOT NULL,
                channel_id INTEGER NOT NULL,
                owner_id INTEGER NOT NULL,
                current_category TEXT
            )
        ''')

        self._connection.commit()

    def close(self) -> None:
        if self._connection:
            self._connection.close()
            logging.info("[DataManager] Database connection closed")

    # =========================================================================
    # TEMP MUTES (persistent across restarts)
    # =========================================================================
    def save_temp_mute(self, mute: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO temp_mutes
                (mute_id, guild_id, user_id, role_id, moderator_id, reason,
                 muted_at, unmute_at, is_active)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                mute.get('mute_id'),
                mute.get('guild_id'),
                mute.get('user_id'),
                mute.get('role_id'),
                mute.get('moderator_id'),
                mute.get('reason'),
                mute.get('muted_at'),
                mute.get('unmute_at'),
                1 if mute.get('is_active', True) else 0,
            ))
            self._connection.commit()

    def get_temp_mute(self, mute_id: str) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM temp_mutes WHERE mute_id = ?', (mute_id,))
        row = cursor.fetchone()
        return dict(row) if row else None

    def deactivate_temp_mute(self, mute_id: str) -> bool:
        """Mark a temp-mute as no longer active. Returns True if a row was updated."""
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute(
                'UPDATE temp_mutes SET is_active = 0 WHERE mute_id = ? AND is_active = 1',
                (mute_id,),
            )
            self._connection.commit()
            return cursor.rowcount > 0

    def load_active_temp_mutes(self) -> List[Dict]:
        """All temp-mutes that are still active (regardless of expiry)."""
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM temp_mutes WHERE is_active = 1')
        return [dict(row) for row in cursor.fetchall()]

    def load_expired_temp_mutes(self, now_iso: str) -> List[Dict]:
        """Active temp-mutes whose unmute_at has passed."""
        cursor = self._connection.cursor()
        cursor.execute(
            'SELECT * FROM temp_mutes WHERE is_active = 1 AND unmute_at <= ?',
            (now_iso,),
        )
        return [dict(row) for row in cursor.fetchall()]

    # === INVITES ===
    def save_invites(self, message_id: Optional[int], channel_id: Optional[int], tracked_invites: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM invites')
            cursor.execute(
                'INSERT INTO invites (message_id, channel_id, tracked_invites) VALUES (?, ?, ?)',
                (message_id, channel_id, pickle.dumps(tracked_invites))
            )
            self._connection.commit()

    def load_invites(self) -> Tuple[Optional[int], Optional[int], Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT message_id, channel_id, tracked_invites FROM invites LIMIT 1')
        row = cursor.fetchone()
        if row:
            tracked = pickle.loads(row['tracked_invites']) if row['tracked_invites'] else {}
            return row['message_id'], row['channel_id'], tracked
        return None, None, {}

    # === INVITE HISTORY (for the "View Previous Invites" button) ===
    def archive_invite_batch(self, invites: Dict[str, Dict[str, Any]], guild_id: Optional[int], archived_by: Optional[int]) -> int:
        """Persist a snapshot of the current tracked invites into history.

        Called when invites are regenerated (or fully expire) so the user can
        browse previous batches later. Returns the number of rows written.
        """
        if not invites:
            return 0
        archived_at = datetime.now(timezone.utc).isoformat()
        rows = []
        for code, data in invites.items():
            rows.append((
                str(_uuid.uuid4())[:8],
                archived_at,
                archived_by,
                code,
                data.get('name'),
                int(data.get('max_uses', 0) or 0),
                int(data.get('uses', 0) or 0),
                data.get('status', 'expired'),
                data.get('created_at'),
                guild_id,
            ))
        with self._lock:
            cursor = self._connection.cursor()
            cursor.executemany(
                '''INSERT INTO invite_history
                   (history_id, archived_at, archived_by, invite_code, invite_name,
                    max_uses, final_uses, status, created_at, guild_id)
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)''',
                rows,
            )
            self._connection.commit()
        return len(rows)

    def load_invite_history(self, guild_id: Optional[int] = None, limit: int = 200) -> List[Dict[str, Any]]:
        """Return invite history rows, newest first."""
        cursor = self._connection.cursor()
        if guild_id is not None:
            cursor.execute(
                'SELECT * FROM invite_history WHERE guild_id = ? ORDER BY archived_at DESC, rowid DESC LIMIT ?',
                (guild_id, limit),
            )
        else:
            cursor.execute(
                'SELECT * FROM invite_history ORDER BY archived_at DESC, rowid DESC LIMIT ?',
                (limit,),
            )
        return [dict(r) for r in cursor.fetchall()]

    def load_invite_history_grouped(self, guild_id: Optional[int] = None, limit: int = 500) -> List[Dict[str, Any]]:
        """Return invite history grouped by archived_at timestamp (newest batch first).

        Each group is a dict: { 'archived_at': str, 'archived_by': int|None, 'invites': [row, ...] }.
        Pagination works on this grouped list (one page = one archived batch).
        """
        rows = self.load_invite_history(guild_id=guild_id, limit=limit)
        groups: List[Dict[str, Any]] = []
        by_ts: Dict[str, Dict[str, Any]] = {}
        for r in rows:
            ts = r.get('archived_at') or 'unknown'
            grp = by_ts.get(ts)
            if grp is None:
                grp = {'archived_at': ts, 'archived_by': r.get('archived_by'), 'invites': []}
                by_ts[ts] = grp
                groups.append(grp)
            grp['invites'].append(r)
        return groups

    def clear_invite_history(self, guild_id: Optional[int] = None) -> int:
        with self._lock:
            cursor = self._connection.cursor()
            if guild_id is not None:
                cursor.execute('DELETE FROM invite_history WHERE guild_id = ?', (guild_id,))
            else:
                cursor.execute('DELETE FROM invite_history')
            self._connection.commit()
            return cursor.rowcount

    # === GETALLROLES MESSAGE TRACKING (auto-updating role list embeds) ===
    def save_getallroles_message(self, message_id: int, channel_id: int, guild_id: int, created_by: Optional[int]) -> None:
        """Record that a getallroles embed was posted, so role-create/delete
        events can find and update it, and the persistent Copy button can be
        re-attached after a restart."""
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute(
                '''INSERT OR REPLACE INTO getallroles_messages
                   (message_id, channel_id, guild_id, created_by, created_at)
                   VALUES (?, ?, ?, ?, ?)''',
                (message_id, channel_id, guild_id, created_by, datetime.now(timezone.utc).isoformat()),
            )
            self._connection.commit()

    def load_getallroles_messages(self, guild_id: Optional[int] = None) -> List[Dict[str, Any]]:
        """Return tracked getallroles embed rows. If guild_id is given, only
        rows for that guild are returned."""
        cursor = self._connection.cursor()
        if guild_id is not None:
            cursor.execute(
                'SELECT * FROM getallroles_messages WHERE guild_id = ? ORDER BY created_at DESC',
                (guild_id,),
            )
        else:
            cursor.execute('SELECT * FROM getallroles_messages ORDER BY created_at DESC')
        return [dict(r) for r in cursor.fetchall()]

    def delete_getallroles_message(self, message_id: int) -> None:
        """Stop tracking a getallroles embed (e.g. the message was deleted)."""
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM getallroles_messages WHERE message_id = ?', (message_id,))
            self._connection.commit()

    # === BLACKLIST ===
    def save_blacklist(self, keywords: Set[str]) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM blacklist')
            cursor.execute('INSERT INTO blacklist (id, keywords) VALUES (1, ?)', (pickle.dumps(keywords),))
            self._connection.commit()

    def load_blacklist(self) -> Set[str]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT keywords FROM blacklist WHERE id = 1')
        row = cursor.fetchone()
        return pickle.loads(row['keywords']) if row and row['keywords'] else set()

    # === RULES CACHE ===
    def save_rules_cache(self, cache: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM rules_cache')
            cursor.execute('''
                INSERT INTO rules_cache (id, gang_rules, server_rules, gang_last_updated, server_last_updated)
                VALUES (1, ?, ?, ?, ?)
            ''', (
                cache.get('gang_rules', ''),
                cache.get('server_rules', ''),
                cache.get('gang_last_updated'),
                cache.get('server_last_updated')
            ))
            self._connection.commit()

    @staticmethod
    def _fix_mojibake(text: str) -> str:
        """
        Fix text that was stored as UTF-8 but read/saved as cp1252 (Windows encoding bug).
        Converts garbled characters like 'âž¢' back to their correct form '➢'.
        Safe to call on already-correct text - it will return it unchanged.
        """
        if not text:
            return text
        try:
            return text.encode('cp1252').decode('utf-8')
        except (UnicodeEncodeError, UnicodeDecodeError):
            return text  # Already correct UTF-8, leave it alone

    def load_rules_cache(self) -> Dict:
        cursor = self._connection.cursor()
        cursor.execute('SELECT gang_rules, server_rules, gang_last_updated, server_last_updated FROM rules_cache WHERE id = 1')
        row = cursor.fetchone()
        if row:
            gang_rules = self._fix_mojibake(row['gang_rules'] or '')
            server_rules = self._fix_mojibake(row['server_rules'] or '')
            self.save_rules_cache({
                'gang_rules': gang_rules,
                'server_rules': server_rules,
                'gang_last_updated': row['gang_last_updated'],
                'server_last_updated': row['server_last_updated']
            })
            return {
                'gang_rules': gang_rules,
                'server_rules': server_rules,
                'gang_last_updated': row['gang_last_updated'],
                'server_last_updated': row['server_last_updated']
            }
        return {'gang_rules': '', 'server_rules': '', 'gang_last_updated': None, 'server_last_updated': None}

    # === LEVELS ===
    def save_level(self, user_id: int, guild_id: int, xp: int, level: int, total_messages: int, last_xp_gain: Optional[datetime]) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO levels (user_id, guild_id, xp, level, total_messages, last_xp_gain)
                VALUES (?, ?, ?, ?, ?, ?)
            ''', (user_id, guild_id, xp, level, total_messages, last_xp_gain.isoformat() if last_xp_gain else None))
            self._connection.commit()

    def load_level(self, user_id: int, guild_id: int) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT xp, level, total_messages, last_xp_gain FROM levels WHERE user_id = ? AND guild_id = ?',
                      (user_id, guild_id))
        row = cursor.fetchone()
        if row:
            return {
                'xp': row['xp'],
                'level': row['level'],
                'total_messages': row['total_messages'],
                'last_xp_gain': datetime.fromisoformat(row['last_xp_gain']) if row['last_xp_gain'] else None
            }
        return None

    def load_all_levels(self) -> Dict[Tuple[int, int], Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT user_id, guild_id, xp, level, total_messages, last_xp_gain FROM levels')
        result = {}
        for row in cursor.fetchall():
            result[(row['user_id'], row['guild_id'])] = {
                'xp': row['xp'],
                'level': row['level'],
                'total_messages': row['total_messages'],
                'last_xp_gain': datetime.fromisoformat(row['last_xp_gain']) if row['last_xp_gain'] else None
            }
        return result

    def save_all_levels(self, levels: Dict[Tuple[int, int], Dict]) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            for (user_id, guild_id), data in levels.items():
                cursor.execute('''
                    INSERT OR REPLACE INTO levels (user_id, guild_id, xp, level, total_messages, last_xp_gain)
                    VALUES (?, ?, ?, ?, ?, ?)
                ''', (
                    user_id, guild_id,
                    data.get('xp', 0),
                    data.get('level', 0),
                    data.get('total_messages', 0),
                    data['last_xp_gain'].isoformat() if data.get('last_xp_gain') else None
                ))
            self._connection.commit()

    # === WARNINGS ===
    def save_warning(self, warning: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO warnings
                (warning_id, user_id, guild_id, moderator_id, warning_type, reason, points, created_at, expires_at, is_active)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                warning.get('warning_id'),
                warning.get('user_id'),
                warning.get('guild_id'),
                warning.get('moderator_id'),
                warning.get('warning_type'),
                warning.get('reason'),
                warning.get('points', 1),
                warning.get('created_at'),
                warning.get('expires_at'),
                1 if warning.get('is_active', True) else 0
            ))
            self._connection.commit()

    def load_warnings(self, guild_id: Optional[int] = None) -> Dict[int, List[Dict]]:
        cursor = self._connection.cursor()
        if guild_id:
            cursor.execute('SELECT * FROM warnings WHERE guild_id = ? AND is_active = 1', (guild_id,))
        else:
            cursor.execute('SELECT * FROM warnings WHERE is_active = 1')

        result: Dict[int, List[Dict]] = {}
        for row in cursor.fetchall():
            user_id = row['user_id']
            if user_id not in result:
                result[user_id] = []
            result[user_id].append({
                'warning_id': row['warning_id'],
                'user_id': row['user_id'],
                'guild_id': row['guild_id'],
                'moderator_id': row['moderator_id'],
                'warning_type': row['warning_type'],
                'reason': row['reason'],
                'points': row['points'],
                'created_at': row['created_at'],
                'expires_at': row['expires_at'],
                'is_active': bool(row['is_active'])
            })
        return result

    def delete_warning(self, warning_id: str) -> bool:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('UPDATE warnings SET is_active = 0 WHERE warning_id = ?', (warning_id,))
            self._connection.commit()
            return cursor.rowcount > 0

    # =========================================================================
    # TICKET TOOL METHODS
    # =========================================================================

    # === TICKET PANELS ===
    def save_ticket_panel(self, panel: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO ticket_panels
                (panel_id, guild_id, channel_id, message_id, name, description, embed_title, 
                 embed_description, embed_color, embed_image, embed_thumbnail, button_style, 
                 button_label, button_emoji, category_id, support_role_id, ticket_limit, 
                 auto_close_hours, welcome_message, claim_required, created_at, is_active)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                panel.get('panel_id'),
                panel.get('guild_id'),
                panel.get('channel_id'),
                panel.get('message_id'),
                panel.get('name'),
                panel.get('description'),
                panel.get('embed_title'),
                panel.get('embed_description'),
                panel.get('embed_color', 0x5865F2),
                panel.get('embed_image'),
                panel.get('embed_thumbnail'),
                panel.get('button_style', 3),
                panel.get('button_label', 'Create Ticket'),
                panel.get('button_emoji'),
                panel.get('category_id'),
                panel.get('support_role_id'),
                panel.get('ticket_limit', 1),
                panel.get('auto_close_hours', 24),
                panel.get('welcome_message'),
                panel.get('claim_required', 0),
                panel.get('created_at'),
                panel.get('is_active', 1)
            ))
            self._connection.commit()

    def load_ticket_panel(self, panel_id: str) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_panels WHERE panel_id = ?', (panel_id,))
        row = cursor.fetchone()
        if row:
            return dict(row)
        return None

    def load_ticket_panels_by_guild(self, guild_id: int) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_panels WHERE guild_id = ? AND is_active = 1', (guild_id,))
        return [dict(row) for row in cursor.fetchall()]

    def load_ticket_panel_by_message(self, message_id: int) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_panels WHERE message_id = ?', (message_id,))
        row = cursor.fetchone()
        if row:
            return dict(row)
        return None

    def delete_ticket_panel(self, panel_id: str) -> bool:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('UPDATE ticket_panels SET is_active = 0 WHERE panel_id = ?', (panel_id,))
            self._connection.commit()
            return cursor.rowcount > 0

    # === TICKETS ===
    def save_ticket(self, ticket: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO tickets
                (ticket_id, guild_id, channel_id, panel_id, creator_id, category, subject, 
                 claimed_by, claimed_at, status, created_at, closed_at, closed_by, 
                 close_reason, rating, rating_feedback)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                ticket.get('ticket_id'),
                ticket.get('guild_id'),
                ticket.get('channel_id'),
                ticket.get('panel_id'),
                ticket.get('creator_id'),
                ticket.get('category'),
                ticket.get('subject'),
                ticket.get('claimed_by'),
                ticket.get('claimed_at'),
                ticket.get('status', 'open'),
                ticket.get('created_at'),
                ticket.get('closed_at'),
                ticket.get('closed_by'),
                ticket.get('close_reason'),
                ticket.get('rating'),
                ticket.get('rating_feedback')
            ))
            self._connection.commit()

    def load_ticket(self, ticket_id: str) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM tickets WHERE ticket_id = ?', (ticket_id,))
        row = cursor.fetchone()
        if row:
            return dict(row)
        return None

    def load_ticket_by_channel(self, channel_id: int) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM tickets WHERE channel_id = ?', (channel_id,))
        row = cursor.fetchone()
        if row:
            return dict(row)
        return None

    def load_tickets_by_guild(self, guild_id: int, status: str = None) -> List[Dict]:
        cursor = self._connection.cursor()
        if status:
            cursor.execute('SELECT * FROM tickets WHERE guild_id = ? AND status = ?', (guild_id, status))
        else:
            cursor.execute('SELECT * FROM tickets WHERE guild_id = ?', (guild_id,))
        return [dict(row) for row in cursor.fetchall()]

    def load_tickets_by_creator(self, creator_id: int, guild_id: int = None) -> List[Dict]:
        cursor = self._connection.cursor()
        if guild_id:
            cursor.execute('SELECT * FROM tickets WHERE creator_id = ? AND guild_id = ? AND status = "open"', (creator_id, guild_id))
        else:
            cursor.execute('SELECT * FROM tickets WHERE creator_id = ? AND status = "open"', (creator_id,))
        return [dict(row) for row in cursor.fetchall()]

    def count_active_tickets_by_creator(self, creator_id: int, guild_id: int = None) -> int:
        """Count a user's ACTIVE tickets (status 'pending' OR 'open').

        This is the authoritative count for the ticket-limit check. Counting
        BOTH 'pending' and 'open' closes the race window where two concurrent
        create requests both passed a check that only counted 'open' tickets
        (the first request had inserted a 'pending' row that the second
        request's check ignored). Combined with the per-user creation lock in
        TicketToolSystem.create_ticket, the limit can no longer be bypassed.
        """
        cursor = self._connection.cursor()
        if guild_id:
            cursor.execute(
                'SELECT COUNT(*) AS c FROM tickets WHERE creator_id = ? AND guild_id = ? AND status IN ("open","pending")',
                (creator_id, guild_id),
            )
        else:
            cursor.execute(
                'SELECT COUNT(*) AS c FROM tickets WHERE creator_id = ? AND status IN ("open","pending")',
                (creator_id,),
            )
        row = cursor.fetchone()
        return int(row['c']) if row else 0

    # ------------------------------------------------------------------
    # Async-friendly wrappers.
    #
    # The DataManager uses synchronous sqlite3. Calling these directly from
    # async Discord handlers blocks the event loop on a busy server. The
    # async_* wrappers below run the same calls in a worker thread via
    # asyncio.to_thread, so the event loop is never blocked. Internally the
    # threading.Lock keeps concurrent access safe.
    # ------------------------------------------------------------------
    async def async_save_ticket(self, ticket: Dict) -> None:
        import asyncio
        await asyncio.to_thread(self.save_ticket, ticket)

    async def async_load_ticket(self, ticket_id: str) -> Optional[Dict]:
        import asyncio
        return await asyncio.to_thread(self.load_ticket, ticket_id)

    async def async_load_ticket_by_channel(self, channel_id: int) -> Optional[Dict]:
        import asyncio
        return await asyncio.to_thread(self.load_ticket_by_channel, channel_id)

    async def async_load_tickets_by_creator(self, creator_id: int, guild_id: int = None) -> List[Dict]:
        import asyncio
        return await asyncio.to_thread(self.load_tickets_by_creator, creator_id, guild_id)

    async def async_save_ticket_answer(self, answer: Dict) -> None:
        import asyncio
        await asyncio.to_thread(self.save_ticket_answer, answer)

    async def async_save_ticket_note(self, note: Dict) -> None:
        import asyncio
        await asyncio.to_thread(self.save_ticket_note, note)

    async def async_save_transcript(self, transcript: Dict) -> None:
        import asyncio
        await asyncio.to_thread(self.save_transcript, transcript)

    async def async_save_ticket_message(self, message: Dict) -> None:
        import asyncio
        await asyncio.to_thread(self.save_ticket_message, message)

    async def async_load_ticket_messages(self, ticket_id: str) -> List[Dict]:
        import asyncio
        return await asyncio.to_thread(self.load_ticket_messages, ticket_id)

    def atomic_claim_ticket(self, ticket_id: str, user_id: int) -> Tuple[bool, str]:
        """
        Atomically claim a ticket via a single conditional UPDATE.

        Returns (success, message). This fixes the TOCTOU race where two staff
        members could both pass the `claimed_by IS NULL` check before either
        save completed.
        """
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute(
                'UPDATE tickets SET claimed_by = ?, claimed_at = ? '
                'WHERE ticket_id = ? AND (claimed_by IS NULL OR claimed_by = 0)',
                (
                    user_id,
                    datetime.now(timezone.utc).isoformat(),
                    ticket_id,
                ),
            )
            self._connection.commit()
            if cursor.rowcount > 0:
                return True, "Ticket claimed."
            # Either the ticket doesn't exist or it's already claimed.
            cursor.execute('SELECT claimed_by FROM tickets WHERE ticket_id = ?', (ticket_id,))
            row = cursor.fetchone()
            if row is None:
                return False, "Ticket not found."
            existing = row['claimed_by'] if isinstance(row, dict) else row[0]
            if existing:
                return False, f"Already claimed by <@{existing}>."
            return False, "Could not claim ticket."

    def atomic_begin_closing(self, ticket_id: str, closed_by: int, reason: str) -> bool:
        """Atomically transition a ticket from 'open' to 'closing'.

        Does a single conditional UPDATE so two near-simultaneous close
        requests can't both pass the `status == 'open'` check and both proceed
        to generate transcripts / delete the channel. Only the first UPDATE
        affects a row (rowcount == 1); the second is a no-op.

        Returns True if this caller won the race (the ticket is now 'closing'
        and owned by this closer), False if the ticket was already closing /
        closed / not found.
        """
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute(
                'UPDATE tickets SET status = ?, closed_by = ?, close_reason = ? '
                'WHERE ticket_id = ? AND status = "open"',
                ('closing', closed_by, reason, ticket_id),
            )
            self._connection.commit()
            return cursor.rowcount > 0

    def atomic_revert_closing(self, ticket_id: str) -> bool:
        """Revert a 'closing' ticket back to 'open'.

        Used when transcript generation fails after atomic_begin_closing
        succeeded, so the ticket can be retried instead of being stuck in
        'closing'. Conditional on status='closing' so it won't clobber a
        concurrent caller that already moved the ticket to 'closed'.
        """
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute(
                'UPDATE tickets SET status = "open", closed_by = NULL, close_reason = NULL '
                'WHERE ticket_id = ? AND status = "closing"',
                (ticket_id,),
            )
            self._connection.commit()
            return cursor.rowcount > 0

    def load_tickets_by_claimed(self, claimed_by: int, guild_id: int = None) -> List[Dict]:
        cursor = self._connection.cursor()
        if guild_id:
            cursor.execute('SELECT * FROM tickets WHERE claimed_by = ? AND guild_id = ? AND status = "open"', (claimed_by, guild_id))
        else:
            cursor.execute('SELECT * FROM tickets WHERE claimed_by = ? AND status = "open"', (claimed_by,))
        return [dict(row) for row in cursor.fetchall()]

    def load_all_open_tickets(self) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM tickets WHERE status = "open"')
        return [dict(row) for row in cursor.fetchall()]

    # === TICKET TRANSCRIPTS ===
    def save_transcript(self, transcript: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT INTO ticket_transcripts
                (transcript_id, ticket_id, guild_id, channel_id, creator_id, closed_by, 
                 claimed_by, category, created_at, closed_at, message_count, file_path, html_content)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                transcript.get('transcript_id'),
                transcript.get('ticket_id'),
                transcript.get('guild_id'),
                transcript.get('channel_id'),
                transcript.get('creator_id'),
                transcript.get('closed_by'),
                transcript.get('claimed_by'),
                transcript.get('category'),
                transcript.get('created_at'),
                transcript.get('closed_at'),
                transcript.get('message_count', 0),
                transcript.get('file_path'),
                transcript.get('html_content')
            ))
            self._connection.commit()

    def load_transcripts_by_guild(self, guild_id: int, limit: int = 50) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_transcripts WHERE guild_id = ? ORDER BY closed_at DESC LIMIT ?', (guild_id, limit))
        return [dict(row) for row in cursor.fetchall()]

    def load_transcript(self, ticket_id: str) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_transcripts WHERE ticket_id = ?', (ticket_id,))
        row = cursor.fetchone()
        if row:
            return dict(row)
        return None

    # === TICKET BLACKLIST ===
    def save_ticket_blacklist(self, blacklist: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO ticket_blacklist
                (blacklist_id, guild_id, user_id, reason, blacklisted_by, blacklisted_at, expires_at, is_active)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                blacklist.get('blacklist_id'),
                blacklist.get('guild_id'),
                blacklist.get('user_id'),
                blacklist.get('reason'),
                blacklist.get('blacklisted_by'),
                blacklist.get('blacklisted_at'),
                blacklist.get('expires_at'),
                blacklist.get('is_active', 1)
            ))
            self._connection.commit()

    def is_user_blacklisted(self, guild_id: int, user_id: int) -> Tuple[bool, Optional[str]]:
        cursor = self._connection.cursor()
        cursor.execute('''
            SELECT * FROM ticket_blacklist 
            WHERE guild_id = ? AND user_id = ? AND is_active = 1
        ''', (guild_id, user_id))
        row = cursor.fetchone()
        if row:
            # Check if expired
            if row['expires_at']:
                expires = datetime.fromisoformat(row['expires_at'])
                if datetime.now(timezone.utc) > expires:
                    return False, None
            return True, row['reason']
        return False, None

    def load_ticket_blacklist(self, guild_id: int) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_blacklist WHERE guild_id = ? AND is_active = 1', (guild_id,))
        return [dict(row) for row in cursor.fetchall()]

    def remove_ticket_blacklist(self, guild_id: int, user_id: int) -> bool:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('UPDATE ticket_blacklist SET is_active = 0 WHERE guild_id = ? AND user_id = ?', (guild_id, user_id))
            self._connection.commit()
            return cursor.rowcount > 0

    # === TICKET SETTINGS ===
    def save_ticket_settings(self, settings: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO ticket_settings
                (guild_id, category_id, transcripts_channel_id, log_channel_id, support_role_id, 
                 admin_role_id, max_tickets_per_user, auto_close_hours, mention_on_create, 
                 dm_transcripts, require_claim, created_at, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                settings.get('guild_id'),
                settings.get('category_id'),
                settings.get('transcripts_channel_id'),
                settings.get('log_channel_id'),
                settings.get('support_role_id'),
                settings.get('admin_role_id'),
                settings.get('max_tickets_per_user', 3),
                settings.get('auto_close_hours', 24),
                settings.get('mention_on_create', 1),
                settings.get('dm_transcripts', 1),
                settings.get('require_claim', 0),
                settings.get('created_at'),
                settings.get('updated_at')
            ))
            self._connection.commit()

    def load_ticket_settings(self, guild_id: int) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_settings WHERE guild_id = ?', (guild_id,))
        row = cursor.fetchone()
        if row:
            return dict(row)
        return None

    # === TICKET QUESTIONS ===
    def save_ticket_question(self, question: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO ticket_questions
                (question_id, panel_id, guild_id, question_text, question_type, required, placeholder, order_index, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                question.get('question_id'),
                question.get('panel_id'),
                question.get('guild_id'),
                question.get('question_text'),
                question.get('question_type', 'text'),
                question.get('required', 1),
                question.get('placeholder'),
                question.get('order_index', 0),
                question.get('created_at')
            ))
            self._connection.commit()

    def load_panel_questions(self, panel_id: str) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_questions WHERE panel_id = ? ORDER BY order_index', (panel_id,))
        return [dict(row) for row in cursor.fetchall()]

    def delete_ticket_question(self, question_id: str) -> bool:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM ticket_questions WHERE question_id = ?', (question_id,))
            self._connection.commit()
            return cursor.rowcount > 0

    # === TICKET ANSWERS ===
    def save_ticket_answer(self, answer: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT INTO ticket_answers (answer_id, ticket_id, question_id, user_id, answer_text, answered_at)
                VALUES (?, ?, ?, ?, ?, ?)
            ''', (
                answer.get('answer_id'),
                answer.get('ticket_id'),
                answer.get('question_id'),
                answer.get('user_id'),
                answer.get('answer_text'),
                answer.get('answered_at')
            ))
            self._connection.commit()

    def load_ticket_answers(self, ticket_id: str) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_answers WHERE ticket_id = ?', (ticket_id,))
        return [dict(row) for row in cursor.fetchall()]

    # === TICKET MESSAGES ===
    def save_ticket_message(self, message: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO ticket_messages
                (message_id, ticket_id, author_id, author_name, author_avatar, content, attachments, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                message.get('message_id'),
                message.get('ticket_id'),
                message.get('author_id'),
                message.get('author_name'),
                message.get('author_avatar'),
                message.get('content'),
                message.get('attachments'),
                message.get('created_at')
            ))
            self._connection.commit()

    def load_ticket_messages(self, ticket_id: str) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_messages WHERE ticket_id = ? ORDER BY created_at', (ticket_id,))
        return [dict(row) for row in cursor.fetchall()]

    def delete_ticket_messages(self, ticket_id: str) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM ticket_messages WHERE ticket_id = ?', (ticket_id,))
            self._connection.commit()

    # === TICKET NOTES ===
    def save_ticket_note(self, note: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO ticket_notes
                (note_id, ticket_id, guild_id, author_id, content, created_at)
                VALUES (?, ?, ?, ?, ?, ?)
            ''', (
                note.get('note_id'),
                note.get('ticket_id'),
                note.get('guild_id'),
                note.get('author_id'),
                note.get('content'),
                note.get('created_at'),
            ))
            self._connection.commit()

    def load_ticket_notes(self, ticket_id: str) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_notes WHERE ticket_id = ? ORDER BY created_at', (ticket_id,))
        return [dict(row) for row in cursor.fetchall()]

    def delete_ticket_note(self, note_id: str) -> bool:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM ticket_notes WHERE note_id = ?', (note_id,))
            self._connection.commit()
            return cursor.rowcount > 0

    # === TICKET STATISTICS ===
    def load_ticket_stats(self, guild_id: int) -> Dict:
        cursor = self._connection.cursor()
        cursor.execute('SELECT COUNT(*) as total FROM tickets WHERE guild_id = ?', (guild_id,))
        total = cursor.fetchone()['total']

        cursor.execute('SELECT COUNT(*) as total FROM tickets WHERE guild_id = ? AND status = "open"', (guild_id,))
        open_count = cursor.fetchone()['total']

        cursor.execute('SELECT COUNT(*) as total FROM tickets WHERE guild_id = ? AND status = "closed"', (guild_id,))
        closed_count = cursor.fetchone()['total']

        cursor.execute('''
            SELECT AVG(rating) as avg_rating FROM tickets
            WHERE guild_id = ? AND rating IS NOT NULL
        ''', (guild_id,))
        row = cursor.fetchone()
        avg_rating = round(row['avg_rating'], 2) if row['avg_rating'] else None

        # Average close time in hours
        cursor.execute('''
            SELECT created_at, closed_at FROM tickets
            WHERE guild_id = ? AND status = "closed" AND closed_at IS NOT NULL
        ''', (guild_id,))
        rows = cursor.fetchall()
        avg_hours = None
        if rows:
            durations = []
            for r in rows:
                try:
                    created = datetime.fromisoformat(r['created_at'].replace('Z', '+00:00'))
                    closed = datetime.fromisoformat(r['closed_at'].replace('Z', '+00:00'))
                    durations.append((closed - created).total_seconds() / 3600)
                except Exception:
                    pass
            if durations:
                avg_hours = round(sum(durations) / len(durations), 2)

        # Priority breakdown
        cursor.execute('''
            SELECT priority, COUNT(*) as cnt FROM tickets
            WHERE guild_id = ? AND status = "open"
            GROUP BY priority
        ''', (guild_id,))
        priority_rows = cursor.fetchall()
        priorities = {r['priority']: r['cnt'] for r in priority_rows}

        return {
            'total': total,
            'open': open_count,
            'closed': closed_count,
            'avg_rating': avg_rating,
            'avg_close_hours': avg_hours,
            'priorities': priorities,
        }

    def update_ticket_first_response(self, ticket_id: str) -> None:
        """Record first staff response time if not already set."""
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute(
                'UPDATE tickets SET first_response_at = ? WHERE ticket_id = ? AND first_response_at IS NULL',
                (datetime.now(timezone.utc).isoformat(), ticket_id)
            )
            self._connection.commit()

    def purge_stale_data(
        self,
        valid_ticket_ids: set,
        valid_panel_ids: set,
        orphaned_open_ticket_ids: set,
        expired_blacklist_ids: set,
        closed_ticket_ids: set,
        purge_closed_ticket_metadata: bool = False,
    ) -> Dict[str, int]:
        """
        Delete rows that are no longer referenced, valid, or needed.
        Returns a dict of table -> rows deleted for the report.

        SAFETY CHANGE: by default (purge_closed_ticket_metadata=False) the
        answers/notes/messages belonging to CLOSED tickets are now PRESERVED,
        not deleted. The previous behavior permanently destroyed the full
        historical record of every closed ticket (only the HTML transcript
        survived). Set purge_closed_ticket_metadata=True to opt back into the
        aggressive cleanup once you're confident it's what you want.
        Transcripts are ALWAYS preserved regardless.
        """
        counts: Dict[str, int] = {}

        with self._lock:
            cursor = self._connection.cursor()

            # 1. Closed tickets. By default we only mark them as already-closed
            # (no-op) and preserve their child data. Only when explicitly opted
            # in do we cascade-delete answers/notes/messages/tickets rows.
            if closed_ticket_ids and purge_closed_ticket_metadata:
                ph = ','.join('?' * len(closed_ticket_ids))
                ids = list(closed_ticket_ids)

                cursor.execute(f'DELETE FROM ticket_answers    WHERE ticket_id IN ({ph})', ids)
                counts['closed_answers'] = cursor.rowcount

                cursor.execute(f'DELETE FROM ticket_notes      WHERE ticket_id IN ({ph})', ids)
                counts['closed_notes'] = cursor.rowcount

                cursor.execute(f'DELETE FROM ticket_messages   WHERE ticket_id IN ({ph})', ids)
                counts['closed_messages'] = cursor.rowcount

                # NOTE: Transcripts are intentionally PRESERVED for closed tickets
                # so staff can reference them later.
                counts['closed_transcripts'] = 0

                cursor.execute(f'DELETE FROM tickets           WHERE ticket_id IN ({ph})', ids)
                counts['closed_tickets'] = cursor.rowcount

                # Rebuild valid_ticket_ids after deleting closed ones
                valid_ticket_ids = valid_ticket_ids - closed_ticket_ids
            else:
                # SAFE DEFAULT: preserve closed-ticket metadata, only flag counts.
                counts['closed_tickets'] = 0
                counts['closed_answers'] = 0
                counts['closed_notes'] = 0
                counts['closed_messages'] = 0
                counts['closed_transcripts'] = 0
                if closed_ticket_ids:
                    logging.info(
                        f"[DataManager] purge_stale_data: preserving metadata for "
                        f"{len(closed_ticket_ids)} closed ticket(s) (opt-in to delete)"
                    )

            # 2. Orphaned child rows whose ticket_id does not exist at all.
            #    (NOT "belongs to a closed ticket" — those are real tickets and
            #    are now preserved by default.)
            if valid_ticket_ids:
                ph2 = ','.join('?' * len(valid_ticket_ids))
                ids2 = list(valid_ticket_ids)

                # Only delete child rows whose ticket_id is NOT a valid ticket
                # at all (truly orphaned), rather than "not in the open set".
                cursor.execute(f'DELETE FROM ticket_answers     WHERE ticket_id NOT IN (SELECT ticket_id FROM tickets)')
                counts['orphan_answers'] = cursor.rowcount

                cursor.execute(f'DELETE FROM ticket_notes       WHERE ticket_id NOT IN (SELECT ticket_id FROM tickets)')
                counts['orphan_notes'] = cursor.rowcount

                cursor.execute(f'DELETE FROM ticket_messages    WHERE ticket_id NOT IN (SELECT ticket_id FROM tickets)')
                counts['orphan_messages'] = cursor.rowcount

                # Transcripts are PRESERVED even when orphaned.
                counts['orphan_transcripts'] = 0
            else:
                # No tickets exist at all in the table — child rows are all
                # orphaned. This branch is the only one that bulk-deletes.
                for tbl in ('ticket_answers', 'ticket_notes', 'ticket_messages'):
                    cursor.execute(f'DELETE FROM {tbl}')
                    counts[f'orphan_{tbl.replace("ticket_", "")}'] = cursor.rowcount
                counts['orphan_transcripts'] = 0

            # 3. Orphaned questions for deleted panels
            if valid_panel_ids:
                ph3 = ','.join('?' * len(valid_panel_ids))
                cursor.execute(
                    f'DELETE FROM ticket_questions WHERE panel_id NOT IN ({ph3})',
                    list(valid_panel_ids)
                )
            else:
                cursor.execute('DELETE FROM ticket_questions')
            counts['orphan_questions'] = cursor.rowcount

            # 4. Mark open tickets as closed where the Discord channel is gone
            if orphaned_open_ticket_ids:
                ph4 = ','.join('?' * len(orphaned_open_ticket_ids))
                cursor.execute(
                    f'''UPDATE tickets SET status = "closed",
                        close_reason = "Channel deleted - cleaned by dbcleanup",
                        closed_at = ?
                        WHERE ticket_id IN ({ph4})''',
                    [datetime.now(timezone.utc).isoformat()] + list(orphaned_open_ticket_ids)
                )
                counts['orphaned_tickets_closed'] = cursor.rowcount
            else:
                counts['orphaned_tickets_closed'] = 0

            # 5. Remove expired / inactive blacklist entries
            if expired_blacklist_ids:
                ph5 = ','.join('?' * len(expired_blacklist_ids))
                cursor.execute(
                    f'DELETE FROM ticket_blacklist WHERE blacklist_id IN ({ph5})',
                    list(expired_blacklist_ids)
                )
                counts['ticket_blacklist'] = cursor.rowcount
            else:
                counts['ticket_blacklist'] = 0

            # 6. Remove deactivated warnings
            cursor.execute('DELETE FROM warnings WHERE is_active = 0')
            counts['warnings'] = cursor.rowcount

            self._connection.commit()

        return counts

    def load_all_tickets(self) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM tickets')
        return [dict(row) for row in cursor.fetchall()]

    def load_all_ticket_panels(self) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_panels')
        return [dict(row) for row in cursor.fetchall()]

    def load_all_ticket_blacklist(self) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM ticket_blacklist')
        return [dict(row) for row in cursor.fetchall()]

    # =========================================================================
    # REACTION ROLES (Carl-bot style, up to 250 per guild)
    # =========================================================================
    MAX_REACTION_ROLES_PER_GUILD = 250

    def save_reaction_role(self, rr: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO reaction_roles
                (mapping_id, guild_id, channel_id, message_id, emoji, role_id,
                 mode, description, created_by, created_at, is_active)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                rr.get('mapping_id'),
                rr.get('guild_id'),
                rr.get('channel_id'),
                rr.get('message_id'),
                rr.get('emoji'),
                rr.get('role_id'),
                rr.get('mode', 'normal'),
                rr.get('description'),
                rr.get('created_by'),
                rr.get('created_at'),
                1 if rr.get('is_active', True) else 0,
            ))
            self._connection.commit()

    def count_reaction_roles(self, guild_id: int) -> int:
        cursor = self._connection.cursor()
        cursor.execute(
            'SELECT COUNT(*) as c FROM reaction_roles WHERE guild_id = ? AND is_active = 1',
            (guild_id,)
        )
        row = cursor.fetchone()
        return row['c'] if row else 0

    def load_reaction_roles_by_guild(self, guild_id: int) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute(
            'SELECT * FROM reaction_roles WHERE guild_id = ? AND is_active = 1 ORDER BY created_at',
            (guild_id,)
        )
        return [dict(row) for row in cursor.fetchall()]

    def load_reaction_roles_by_message(self, message_id: int) -> List[Dict]:
        cursor = self._connection.cursor()
        cursor.execute(
            'SELECT * FROM reaction_roles WHERE message_id = ? AND is_active = 1',
            (message_id,)
        )
        return [dict(row) for row in cursor.fetchall()]

    def find_reaction_role(self, message_id: int, emoji: str) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute(
            'SELECT * FROM reaction_roles WHERE message_id = ? AND emoji = ? AND is_active = 1',
            (message_id, emoji)
        )
        row = cursor.fetchone()
        return dict(row) if row else None

    def delete_reaction_role(self, mapping_id: str) -> bool:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute(
                'DELETE FROM reaction_roles WHERE mapping_id = ?', (mapping_id,)
            )
            self._connection.commit()
            return cursor.rowcount > 0

    def delete_reaction_roles_for_message(self, message_id: int) -> int:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute(
                'DELETE FROM reaction_roles WHERE message_id = ?', (message_id,)
            )
            self._connection.commit()
            return cursor.rowcount

    # =========================================================================
    # STICKY ROLES (Dyno premium — re-apply on rejoin)
    # =========================================================================
    def get_sticky_role_config(self, guild_id: int) -> Dict:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM sticky_role_config WHERE guild_id = ?', (guild_id,))
        row = cursor.fetchone()
        if not row:
            return {
                'guild_id': guild_id, 'enabled': 0,
                'eligible_role_ids': '[]', 'updated_at': None,
            }
        return dict(row)

    def save_sticky_role_config(self, cfg: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO sticky_role_config
                (guild_id, enabled, eligible_role_ids, updated_at)
                VALUES (?, ?, ?, ?)
            ''', (
                cfg.get('guild_id'),
                1 if cfg.get('enabled') else 0,
                cfg.get('eligible_role_ids', '[]'),
                cfg.get('updated_at') or datetime.now(timezone.utc).isoformat(),
            ))
            self._connection.commit()

    def save_sticky_roles(self, guild_id: int, user_id: int, role_ids: List[int]) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO sticky_roles
                (entry_id, guild_id, user_id, role_ids, saved_at)
                VALUES (?, ?, ?, ?, ?)
            ''', (
                str(_uuid.uuid4()), guild_id, user_id,
                json.dumps(role_ids),
                datetime.now(timezone.utc).isoformat(),
            ))
            self._connection.commit()

    def load_sticky_roles(self, guild_id: int, user_id: int) -> List[int]:
        cursor = self._connection.cursor()
        cursor.execute(
            'SELECT role_ids FROM sticky_roles WHERE guild_id = ? AND user_id = ?',
            (guild_id, user_id)
        )
        row = cursor.fetchone()
        if not row:
            return []
        try:
            return [int(r) for r in json.loads(row['role_ids'])]
        except Exception:
            return []

    def delete_sticky_roles(self, guild_id: int, user_id: int) -> bool:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute(
                'DELETE FROM sticky_roles WHERE guild_id = ? AND user_id = ?',
                (guild_id, user_id)
            )
            self._connection.commit()
            return cursor.rowcount > 0

    # =========================================================================
    # FULL MESSAGE LOGGING (Dyno premium — edits + deletes with content)
    # =========================================================================
    def get_message_log_config(self, guild_id: int) -> Dict:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM message_log_config WHERE guild_id = ?', (guild_id,))
        row = cursor.fetchone()
        if not row:
            return {
                'guild_id': guild_id, 'enabled': 0, 'log_channel_id': None,
                'log_edits': 1, 'log_deletes': 1, 'ignore_bots': 1,
                'ignore_channels': '[]', 'updated_at': None,
            }
        return dict(row)

    def save_message_log_config(self, cfg: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO message_log_config
                (guild_id, enabled, log_channel_id, log_edits, log_deletes,
                 ignore_bots, ignore_channels, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                cfg.get('guild_id'),
                1 if cfg.get('enabled') else 0,
                cfg.get('log_channel_id'),
                1 if cfg.get('log_edits', True) else 0,
                1 if cfg.get('log_deletes', True) else 0,
                1 if cfg.get('ignore_bots', True) else 0,
                cfg.get('ignore_channels', '[]'),
                cfg.get('updated_at') or datetime.now(timezone.utc).isoformat(),
            ))
            self._connection.commit()

    def cache_message(self, msg: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO message_log_cache
                (message_id, guild_id, channel_id, author_id, author_name,
                 content, attachments, created_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                msg.get('message_id'), msg.get('guild_id'), msg.get('channel_id'),
                msg.get('author_id'), msg.get('author_name'),
                msg.get('content', ''), msg.get('attachments', '[]'),
                msg.get('created_at'),
            ))
            self._connection.commit()

    def load_cached_message(self, message_id: int) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM message_log_cache WHERE message_id = ?', (message_id,))
        row = cursor.fetchone()
        return dict(row) if row else None

    def delete_cached_message(self, message_id: int) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM message_log_cache WHERE message_id = ?', (message_id,))
            self._connection.commit()

    def prune_message_cache(self, keep_recent: int = 5000) -> int:
        """Keep the message cache from growing without bound."""
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('SELECT COUNT(*) as c FROM message_log_cache')
            total = cursor.fetchone()['c']
            if total <= keep_recent:
                return 0
            cursor.execute('''
                DELETE FROM message_log_cache WHERE message_id IN (
                    SELECT message_id FROM message_log_cache
                    ORDER BY created_at ASC LIMIT ?
                )
            ''', (total - keep_recent,))
            self._connection.commit()
            return cursor.rowcount

    # CUSTOM BOT BRANDING (avatar / banner / embed footer override)
    def get_branding(self, guild_id: int) -> Dict:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM bot_branding WHERE guild_id = ?', (guild_id,))
        row = cursor.fetchone()
        if not row:
            return {
                'guild_id': guild_id, 'embed_footer': None, 'embed_color': None,
                'embed_thumbnail': None, 'embed_image': None,
                'avatar_url': None, 'banner_url': None, 'updated_at': None,
            }
        return dict(row)

    def save_branding(self, branding: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO bot_branding
                (guild_id, embed_footer, embed_color, embed_thumbnail,
                 embed_image, avatar_url, banner_url, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                branding.get('guild_id'),
                branding.get('embed_footer'),
                branding.get('embed_color'),
                branding.get('embed_thumbnail'),
                branding.get('embed_image'),
                branding.get('avatar_url'),
                branding.get('banner_url'),
                branding.get('updated_at') or datetime.now(timezone.utc).isoformat(),
            ))
            self._connection.commit()

    def save_ows_panel_state(self, message_id: int, channel_id: int, owner_id: int, current_category: str) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO ows_active_panel (id, message_id, channel_id, owner_id, current_category)
                VALUES (1, ?, ?, ?, ?)
            ''', (message_id, channel_id, owner_id, current_category))
            self._connection.commit()

    def load_ows_panel_state(self) -> Optional[Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT message_id, channel_id, owner_id, current_category FROM ows_active_panel WHERE id = 1')
        row = cursor.fetchone()
        if row:
            return dict(row)
        return None

    def delete_ows_panel_state(self) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM ows_active_panel WHERE id = 1')
            self._connection.commit()

    # BOT CONFIG (Branding & Channels)
    def get_config_value(self, key: str) -> Optional[str]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT value FROM bot_config WHERE key = ?', (key,))
        row = cursor.fetchone()
        return row['value'] if row else None

    def set_config_value(self, key: str, value: str) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('INSERT OR REPLACE INTO bot_config (key, value) VALUES (?, ?)', (key, value))
            self._connection.commit()

    # GIVEAWAYS
    def save_giveaway(self, giveaway: Dict) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('''
                INSERT OR REPLACE INTO giveaways
                (giveaway_id, message_id, channel_id, guild_id, host_id, prize, winner_count,
                 entries, winners, status, created_at, ends_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ''', (
                giveaway.get('giveaway_id'),
                giveaway.get('message_id'),
                giveaway.get('channel_id'),
                giveaway.get('guild_id'),
                giveaway.get('host_id'),
                giveaway.get('prize'),
                giveaway.get('winner_count', 1),
                json.dumps(giveaway.get('entries', [])),
                json.dumps(giveaway.get('winners', [])),
                giveaway.get('status', 'active'),
                giveaway.get('created_at'),
                giveaway.get('ends_at')
            ))
            self._connection.commit()

    def load_all_giveaways(self) -> Dict[str, Dict]:
        cursor = self._connection.cursor()
        cursor.execute('SELECT * FROM giveaways')
        result = {}
        for row in cursor.fetchall():
            result[row['giveaway_id']] = {
                'giveaway_id': row['giveaway_id'],
                'message_id': row['message_id'],
                'channel_id': row['channel_id'],
                'guild_id': row['guild_id'],
                'host_id': row['host_id'],
                'prize': row['prize'],
                'winner_count': row['winner_count'],
                'entries': json.loads(row['entries']) if row['entries'] else [],
                'winners': json.loads(row['winners']) if row['winners'] else [],
                'status': row['status'],
                'created_at': row['created_at'],
                'ends_at': row['ends_at']
            }
        return result

    def delete_giveaway(self, giveaway_id: str) -> None:
        with self._lock:
            cursor = self._connection.cursor()
            cursor.execute('DELETE FROM giveaways WHERE giveaway_id = ?', (giveaway_id,))
            self._connection.commit()

# --- DATA MODELS ---
@dataclass
class Warning:
    warning_id: str = ""
    user_id: int = 0
    guild_id: int = 0
    moderator_id: int = 0
    warning_type: WarningType = WarningType.CUSTOM
    reason: str = ""
    points: int = 1
    created_at: datetime = None
    expires_at: datetime = None
    is_active: bool = True
    
    def __post_init__(self):
        if not self.warning_id:
            import uuid
            self.warning_id = str(uuid.uuid4())[:8]
        if self.created_at is None:
            self.created_at = datetime.now(timezone.utc)


@dataclass
class Ticket:
    ticket_id: str = ""
    channel_id: int = 0
    guild_id: int = 0
    creator_id: int = 0
    category: str = "general"
    subject: str = ""
    status: TicketStatus = TicketStatus.OPEN
    assigned_to: int = None
    created_at: datetime = None
    
    def __post_init__(self):
        if not self.ticket_id:
            import uuid
            self.ticket_id = str(uuid.uuid4())[:8]
        if self.created_at is None:
            self.created_at = datetime.now(timezone.utc)


@dataclass
class Giveaway:
    giveaway_id: str = ""
    message_id: int = 0
    channel_id: int = 0
    guild_id: int = 0
    host_id: int = 0
    prize: str = ""
    winner_count: int = 1
    entries: List[int] = None
    winners: List[int] = None
    status: GiveawayStatus = GiveawayStatus.ACTIVE
    created_at: datetime = None
    ends_at: datetime = None
    
    def __post_init__(self):
        if not self.giveaway_id:
            import uuid
            self.giveaway_id = str(uuid.uuid4())[:8]
        if self.created_at is None:
            self.created_at = datetime.now(timezone.utc)
        if self.entries is None:
            self.entries = []
        if self.winners is None:
            self.winners = []


@dataclass
class UserLevel:
    user_id: int = 0
    guild_id: int = 0
    xp: int = 0
    level: int = 0
    total_messages: int = 0
    last_xp_gain: datetime = None
    
    @property
    def xp_for_next_level(self) -> int:
        # Delegates to the centralized curve helper so the curve never drifts.
        return xp_for_next_level(self.level)

    @property
    def xp_progress(self) -> float:
        xp_needed = self.xp_for_next_level
        current_level_xp = xp_for_level(self.level)
        xp_in_current_level = self.xp - current_level_xp
        xp_for_next = xp_needed - current_level_xp
        return min(100.0, (xp_in_current_level / xp_for_next) * 100) if xp_for_next > 0 else 100.0

    def add_xp(self, amount: int) -> bool:
        self.xp += amount
        self.total_messages += 1
        # Recompute the level from total XP so multiple level-ups in one gain
        # are handled correctly.
        new_level = compute_level_from_xp(self.xp)
        if new_level > self.level:
            self.level = new_level
            return True
        return False


# --- GLOBAL INSTANCE ---
config = Config()
config.ensure_directories()
data_manager = DataManager(config.db_file)


# --- LOGGING SETUP ---
def setup_logging() -> None:
    log_formatter = logging.Formatter('%(asctime)s - %(levelname)s - %(message)s')
    file_handler = RotatingFileHandler(
        'bot.log',
        maxBytes=1024*1024,
        backupCount=5,
        encoding='utf-8'
    )
    file_handler.setFormatter(log_formatter)
    stream_handler = logging.StreamHandler()
    stream_handler.setFormatter(log_formatter)
    logging.basicConfig(level=logging.INFO, handlers=[file_handler, stream_handler])


setup_logging()


# =============================================================================
# SHARED HELPERS
# =============================================================================
# --- XP / Leveling curve (centralized so the curve can never drift) ---
# Gentler curve: level N requires (N ** 1.5) * 100 cumulative XP.
# At 5-15 XP/message this keeps leveling achievable instead of taking
# thousands of messages to reach mid levels.
XP_CURVE_BASE: int = 100
XP_CURVE_EXPONENT: float = 1.5


def xp_for_level(level: int) -> int:
    """Total cumulative XP required to REACH a given level (level 0 == 0 XP)."""
    if level <= 0:
        return 0
    return int(level ** XP_CURVE_EXPONENT * XP_CURVE_BASE)


def xp_for_next_level(level: int) -> int:
    """Total cumulative XP required to reach the level AFTER `level`."""
    return xp_for_level(level + 1)


def compute_level_from_xp(xp: int) -> int:
    """Given total XP, return the highest level the user has achieved."""
    level = 0
    while xp >= xp_for_next_level(level):
        level += 1
        # Safety valve to avoid an infinite loop on pathological input.
        if level > 10_000:
            break
    return level


def _is_negative_answer(text: str) -> bool:
    """
    Word-boundary-safe check for a negative / empty answer.
    Fixes the old substring bug where 'na' matched 'natural', 'banana',
    'nathan', 'not' matched 'noted', etc.
    """
    if text is None:
        return True
    normalized = re.sub(r"[^a-z0-9\s/]", " ", text.lower())
    normalized = re.sub(r"\s+", " ", normalized).strip()
    if not normalized:
        return True

    negative_words = {
        "no", "none", "na", "n/a", "not", "idk", "nil", "nope",
        "nah", "negative", "nothing",
    }
    words = set(normalized.split())
    if words & negative_words:
        return True

    # Multi-word negative phrases.
    for phrase in ("not sure", "i dont know", "i don t know",
                   "none of the above", "dont know", "don t know"):
        if phrase in normalized:
            return True
    return False


def brand_text(text: str) -> str:
    """
    Rebrand hardcoded legacy names AND [GANG NAME]/[GANG ABBR] placeholders
    with the configured gang name / abbreviation.

    Uses unique sentinel placeholders so that a configured gang name or
    abbreviation containing a legacy token can never re-trigger another
    replacement and corrupt the text.
    """
    if not text:
        return text

    # Sentinels for legacy tokens
    PH_FULL_1 = "\x00\x01BRAND_FULL_1\x01\x00"   # "Mask Off Society"
    PH_FULL_2 = "\x00\x01BRAND_FULL_2\x01\x00"   # "Shoot On Sight"
    PH_FULL_3 = "\x00\x01BRAND_FULL_3\x01\x00"   # "SOS"
    PH_ABBR   = "\x00\x01BRAND_ABBR\x01\x00"     # "MOS"
    PH_SERVER = "\x00\x01BRAND_SERVER\x01\x00"   # "VPRP"
    # Sentinels for [GANG NAME] / [GANG ABBR] placeholders
    PH_PH_FULL = "\x00\x01BRAND_PH_FULL\x01\x00"
    PH_PH_ABBR = "\x00\x01BRAND_PH_ABBR\x01\x00"

    # 1) Replace every token with a unique sentinel.
    result = text.replace("Mask Off Society", PH_FULL_1)
    result = result.replace("Shoot On Sight", PH_FULL_2)
    result = result.replace("[GANG NAME]", PH_PH_FULL)       # NEW
    result = result.replace("[GANG ABBR]", PH_PH_ABBR)       # NEW
    result = result.replace("SOS", PH_FULL_3)
    result = result.replace("MOS", PH_ABBR)
    result = result.replace("VPRP", PH_SERVER)

    # 2) Resolve sentinels to configured values exactly once.
    result = result.replace(PH_FULL_1, config.gang_name)
    result = result.replace(PH_FULL_2, config.gang_name)
    result = result.replace(PH_FULL_3, config.gang_name)
    result = result.replace(PH_PH_FULL, config.gang_name)   # NEW
    result = result.replace(PH_PH_ABBR, config.gang_abbreviation)  # NEW
    result = result.replace(PH_ABBR, config.gang_abbreviation)
    result = result.replace(PH_SERVER, "Server")
    return result


# --- BOT INTENTS AND INSTANCE ---
intents = discord.Intents.default()
intents.members = True
intents.message_content = True
intents.invites = True
intents.presences = True
intents.reactions = True


class TicketBot(commands.Bot):
    """commands.Bot subclass with a one-time setup_hook.

    discord.py calls setup_hook() EXACTLY ONCE during startup, before
    on_ready. This is the correct place for DB initialization and other
    one-time work that must not repeat when the gateway reconnects and
    on_ready fires again.

    Previously, data_manager.connect() and several load_*() calls lived
    inside on_ready. Because on_ready can fire multiple times (on every
    gateway reconnect), those calls would re-run — and even though
    DataManager.connect() is now idempotent (it early-returns if already
    connected), re-running the loads is wasteful and re-registering
    persistent views is noisy. setup_hook eliminates the concern entirely.
    """

    async def setup_hook(self) -> None:
        # --- One-time DB + cache initialization (runs exactly once) ---
        data_manager.connect()                # idempotent: safe even if called again
        hydrate_ows_settings()
        config.load_branding_settings()
        config.load_channel_settings()

        # Load persisted state from the (now-connected) SQLite DB into the
        # in-memory caches. These must run before on_ready so the caches
        # are populated before the first guild event arrives.
        load_blacklist_data()
        load_rules_cache()
        load_giveaways_data()
        load_levels_data()

        # One-time JSON → SQLite migration (if enabled and not already done).
        if ows_get("json_to_sqlite_import"):
            import_json_to_sqlite()

        # Pre-register the generic persistent views (the per-panel and
        # per-giveaway ones are registered in on_ready, where self.guilds is
        # available — setup_hook runs before guilds are fully cached).
        self.add_view(VerificationButtonsView())
        self.add_view(ImprovedStaffActionView(0, "", discord.Embed()))
        self.add_view(TicketControlView(""))
        self.add_view(InviteTrackingView(None))
        self.add_view(GetAllRolesView())
        logging.info("[SetupHook] One-time initialization complete (DB + caches + generic views)")


bot = TicketBot(command_prefix=config.command_prefix, intents=intents)

# Once-flag: ensure slash commands are synced only once per process.
_slash_synced: bool = False

# Once-flag: ensure the one-time portion of on_ready runs only on the first
# connect, not on every gateway reconnect. (setup_hook handles DB + caches;
# this flag guards the guild-dependent work in on_ready that can't move to
# setup_hook because it needs bot.guilds to be fully cached.)
_on_ready_initialized: bool = False


# --- GLOBAL STATE ---
verification_flags: Dict[int, bool] = {}
verification_cooldowns: Dict[int, float] = {}
messages_enabled: bool = True
start_time: float = time.time()
invite_manager: Optional['InviteManager'] = None
blacklisted_keywords: Set[str] = set()
rules_cache: Dict[str, Any] = {'gang_rules': '', 'server_rules': '', 'gang_last_updated': None, 'server_last_updated': None}

# New V2 data stores
warnings_data: Dict[int, List[Dict]] = {}  # guild_id -> list of warnings
# NOTE: tickets are persisted entirely in SQLite via data_manager (no in-memory cache).
giveaways_data: Dict[str, Dict] = {}  # giveaway_id -> giveaway data (in-memory)
levels_data: Dict[Tuple[int, int], Dict] = {}  # (user_id, guild_id) -> level data


# --- PROCESS MANAGER ---
class ProcessManager:
    def __init__(self):
        self._active_verifications: Set[int] = set()
        self._active_processes: int = 0
    
    def add_verification(self, user_id: int) -> None:
        self._active_verifications.add(user_id)
        self._update_lock_file()
    
    def remove_verification(self, user_id: int) -> None:
        self._active_verifications.discard(user_id)
        self._update_lock_file()
    
    def is_busy(self) -> bool:
        return len(self._active_verifications) > 0 or self._active_processes > 0
    
    def _update_lock_file(self) -> None:
        if not ows_get("process_lock_file"):
            return
        try:
            if self.is_busy():
                with open(config.lock_file, 'w') as f:
                    f.write(f"busy since: {datetime.now().isoformat()}")
            else:
                if os.path.exists(config.lock_file):
                    os.remove(config.lock_file)
        except Exception as e:
            logging.error(f"Error updating lock file: {e}")
    
    def clear_lock_file(self) -> None:
        try:
            if os.path.exists(config.lock_file):
                os.remove(config.lock_file)
        except Exception as e:
            logging.error(f"Error removing lock file: {e}")


process_manager = ProcessManager()


# --- TEMPLATES ---
VERIFICATION_QUESTIONS: List[Tuple[str, str]] = [
    ("Before we start, do you agree to the rules and expectations of our community?", "Agreement"),
    ("What is your age?", "Age"),
    ("What other gangs are you affiliated with? (Reply 'Skip' if you're an ally)", "Gang Affiliations"),
    ("Send a screenshot of the member who invited you. (Reply 'Skip' if you're an ally)", "Invitation Proof"),
    ("How good would you say your aim is, and do you use any aim trainers in your spare time?", "Aim Skill & Training"),
    ("Do you have any previous experience with gangs [Ex: FiveM, Ro hood rp, Street shooters, Etc..]?", "Previous Gang Experience"),
    ("What is your Discord username?", "Discord Username"),
    ("What do you want your gang name to be? (Format: [GANG NAME]_YourGangName)", "Desired Gang Name"),
    ("How was your experience with our verification system?", "Feedback")
]

# Enhanced V2 questions used by ImprovedVerificationSystem
VERIFICATION_QUESTIONS_V2: List[Dict[str, Any]] = [
    {
        "question": "Before we start, do you agree to the community rules and expectations?",
        "label": "Agreement",
        "hint": "Review the rules, then type 'I agree'",
        "is_agreement": True,
        "skip_allowed": False
    },
    {
        "question": "What is your age?",
        "label": "Age",
        "hint": "Please provide your real age. This helps us understand our Community.",
        "is_agreement": False,
        "skip_allowed": False
    },
    {
        "question": "What other Gangs are you affiliated with?",
        "label": "Gang Affiliations",
        "hint": "Reply 'skip' if you're an ally or not affiliated with any gangs.",
        "is_agreement": False,
        "skip_allowed": True
    },
    {
        "question": "Send a screenshot of the member who invited you.",
        "label": "Invitation Proof",
        "hint": "You can upload an image or paste a link. Reply 'skip' if you're an ally.",
        "is_agreement": False,
        "skip_allowed": True,
        "accepts_attachment": True
    },
    {
        "question": "How good would you say your aim/fighting skills are, and do you use any aim/fist trainers?",
        "label": "Aim/Fight Skill & Training",
        "hint": "Be honest! We accept players of all skill levels.",
        "is_agreement": False,
        "skip_allowed": False
    },
    {
        "question": "Do you have any previous experience with gangs? (FiveM, Ro Hood RP, Street Shooters, etc.)",
        "label": "Previous Gang Experience",
        "hint": "Previous experience is not required, but it helps us know your background.",
        "is_agreement": False,
        "skip_allowed": True
    },
    {
        "question": "Have you ever been part of a Leading role inside of a Gang Faction specifically? (Further questions may be asked if so)",
        "label": "Gang Leadership Experience",
        "hint": "Answer yes if you've held a leadership or organizer role in a gang faction.",
        "is_agreement": False,
        "skip_allowed": False,
        "depends_on_label": "Previous Gang Experience"
    },
    {
        "question": "What is your Roblox User?",
        "label": "Roblox Username",
        "hint": "Provide your Roblox username so staff can verify your account if needed.",
        "is_agreement": False,
        "skip_allowed": False
    },
    {
        "question": "What is your Discord username?",
        "label": "Discord Username",
        "hint": "Example: username#1234 or just username for new Discord format",
        "is_agreement": False,
        "skip_allowed": False
    },
    {
        "question": "What do you want your gang name to be?",
        "label": "Desired Gang Name",
        "hint": "Format: [GANG NAME]_YourGangName (e.g., [GANG NAME]_Shadows)",
        "is_agreement": False,
        "skip_allowed": False
    },
    {
        "question": "How was your experience with our verification system?",
        "label": "Feedback",
        "hint": "Your feedback helps us improve! Let us know what you think.",
        "is_agreement": False,
        "skip_allowed": True
    }
]

INVITE_CONFIGS: List[Dict[str, Any]] = [
    {"name": "10 Uses", "max_uses": 10, "max_age": 0},
    {"name": "25 Uses", "max_uses": 25, "max_age": 0},
    {"name": "50 Uses", "max_uses": 50, "max_age": 0},
]

REPORT_TEMPLATES: List[str] = [
    "Ay, someone pass me a blunt. Damn.. no one? Whatever. If someone is disrespecting members or allies, type '!report <Ping the User> <Reason Here>'",
    "Ay, y'all wanna slide on the opps?",
    "Y'all know it's [GANG NAME] business. **[GANG NAME] ON TOP!**"
]

WELCOME_TEMPLATES: List[str] = [
    "Welcome {mention} to [GANG NAME]. We expect you to put in work.",
    "What's good, {mention}? Brought any Pizza? No? Whatever. Welcome to [GANG NAME], we expect you to work hard.",
    "Yoooo big dawg {mention} just dropped in, what's good?",
    "Is that who I think it is? {mention}, welcome to [GANG NAME]. We expect you to put in work.",
    "Ay {mention}, we gotta Recruit soon, let me know when you're free Dawg.",
    "Well, well, well, if it isn't the one and only {mention}. We're glad to have you around."
]

REPORT_CATEGORIES: Dict[str, str] = {
    "1️⃣": "Rule Violation",
    "2️⃣": "Harassment",
    "3️⃣": "Scamming",
    "4️⃣": "Cheating",
    "5️⃣": "Other"
}

TICKET_CATEGORIES: Dict[str, str] = {
    "general": "General Support",
    "report": "Player Report",
    "appeal": "Ban Appeal",
    "verification": "Verification Help",
    "other": "Other",
    "alliance": "Request an Alliance",
    "opp": "Request us to Add opp gangs / players"
}

# Replace your corrupted emoji list with this:
POLL_EMOJIS = [
    "1️⃣",  # 1️⃣
    "2️⃣",  # 2️⃣
    "3️⃣",  # 3️⃣
    "4️⃣",  # 4️⃣
    "5️⃣",  # 5️⃣
    "6️⃣",  # 6️⃣
    "7️⃣",  # 7️⃣
    "8️⃣",  # 8️⃣
    "9️⃣",  # 9️⃣
]

# --- EMBED BUILDER (V2 Enhancement) ---
class EmbedBuilder:
    @staticmethod
    def success(title: str, description: str) -> discord.Embed:
        return discord.Embed(title=title, description=description, color=discord.Color.green(), timestamp=datetime.now(timezone.utc))
    
    @staticmethod
    def error(title: str, description: str) -> discord.Embed:
        return discord.Embed(title=title, description=description, color=discord.Color.red(), timestamp=datetime.now(timezone.utc))
    
    @staticmethod
    def warning(title: str, description: str) -> discord.Embed:
        return discord.Embed(title=title, description=description, color=discord.Color.orange(), timestamp=datetime.now(timezone.utc))
    
    @staticmethod
    def info(title: str, description: str) -> discord.Embed:
        return discord.Embed(title=title, description=description, color=discord.Color.blue(), timestamp=datetime.now(timezone.utc))
    
    @staticmethod
    def verification(title: str, description: str) -> discord.Embed:
        return discord.Embed(title=title, description=description, color=discord.Color.gold(), timestamp=datetime.now(timezone.utc))
    
    @staticmethod
    def ticket(title: str, description: str) -> discord.Embed:
        return discord.Embed(title=title, description=description, color=discord.Color.blurple(), timestamp=datetime.now(timezone.utc))
    
    @staticmethod
    def giveaway(title: str, description: str) -> discord.Embed:
        return discord.Embed(title=title, description=description, color=discord.Color.purple(), timestamp=datetime.now(timezone.utc))
    
    @staticmethod
    def level(user: discord.Member, level_data: Dict) -> discord.Embed:
        embed = discord.Embed(title=f"📊 Level Card: {user.display_name}", color=discord.Color.gold())
        if user.avatar:
            embed.set_thumbnail(url=user.avatar.url)
        
        level = level_data.get('level', 0)
        xp = level_data.get('xp', 0)
        total_messages = level_data.get('total_messages', 0)

        xp_for_next = xp_for_next_level(level)
        current_xp = xp_for_level(level)
        progress = min(100.0, ((xp - current_xp) / (xp_for_next - current_xp)) * 100) if xp_for_next > current_xp else 100.0
        
        progress_bar = "█" * int(progress / 10) + "░" * (10 - int(progress / 10))        
        embed.add_field(name="Level", value=f"**{level}**", inline=True)
        embed.add_field(name="XP", value=f"{xp:,}", inline=True)
        embed.add_field(name="Progress", value=f"`{progress_bar}` {progress:.1f}%", inline=True)
        embed.add_field(name="Messages", value=f"{total_messages:,}", inline=True)
        
        return embed

    # ------------------------------------------------------------------
    # Custom Bot Branding support (avatar / banner / footer override)
    # ------------------------------------------------------------------
    @staticmethod
    def branded(base_embed: discord.Embed, guild_id: Optional[int]) -> discord.Embed:
        """Apply per-guild custom branding (footer / color / thumbnail / image)
        to an existing embed. Falls back gracefully if no branding is configured
        or the data manager isn't ready yet (called very early in startup)."""
        try:
            if data_manager is None or data_manager._connection is None:
                return base_embed
            if guild_id is None:
                return base_embed
            branding = data_manager.get_branding(guild_id)
            footer = branding.get('embed_footer')
            if footer:
                base_embed.set_footer(text=brand_text(footer))
            color = branding.get('embed_color')
            if isinstance(color, int):
                base_embed.colour = discord.Color(color)
            thumb = branding.get('embed_thumbnail')
            if thumb:
                base_embed.set_thumbnail(url=thumb)
            img = branding.get('embed_image')
            if img:
                base_embed.set_image(url=img)
        except Exception as exc:
            logging.debug(f"[EmbedBuilder.branded] skipped branding: {exc}")
        return base_embed


# =============================================================================
# VERIFICATION EMBED BUILDER - Consistent, Modern Design
# =============================================================================

class VerificationEmbedBuilder:
    """
    Centralized embed builder for verification system.
    Ensures consistent branding and visual design across all verification messages.
    """

    COLOR_PRIMARY = 0x5865F2
    COLOR_SUCCESS = 0x57F287
    COLOR_WARNING = 0xFEE75C
    COLOR_ERROR   = 0xED4245
    COLOR_INFO    = 0x3498DB
    COLOR_GANG    = 0xFF6B35
    COLOR_GOLD    = 0xFFD700

    @staticmethod
    def create_base_embed(title: str, description: str = "", color: int = None,
                          thumbnail_url: str = None, show_brand: bool = True) -> discord.Embed:
        embed = discord.Embed(
            title=title, description=description,
            color=color or VerificationEmbedBuilder.COLOR_PRIMARY,
            timestamp=datetime.now(timezone.utc)
        )
        if show_brand:
            embed.set_footer(text=f"{config.gang_name} Verification System")
        if thumbnail_url:
            embed.set_thumbnail(url=thumbnail_url)
        return embed

    @staticmethod
    def welcome_screen(user: discord.User) -> discord.Embed:
        return VerificationEmbedBuilder.create_base_embed(
            title=f"🎭 **{config.gang_name.upper()} VERIFICATION PORTAL**",
            description=(
                f"Welcome **{user.display_name}**!\n\n"
                f"You're about to begin the **{config.gang_name}** verification process.\n"
                "This will take approximately **5-10 minutes** to complete.\n\n"
                "┌─────────────────────────────┐\n"
                "│  **What to Expect:**        │\n"
                "│  • Review our server rules  │\n"
                "│  • Answer a few questions   │\n"
                "│  • Complete the server steps │\n"
                "│  • Submit for staff review  │\n"
                "└─────────────────────────────┘\n\n"
                "Press **START** when you're ready!"
            ),
            color=VerificationEmbedBuilder.COLOR_GANG,
            thumbnail_url=user.avatar.url if user.avatar else user.default_avatar.url
        )

    @staticmethod
    def rules_intro() -> discord.Embed:
        embed = VerificationEmbedBuilder.create_base_embed(
            title="📜 **RULES AGREEMENT**",
            description=(
                f"Before joining **{config.gang_name}**, you must review and agree "
                "to follow the rules of our community.\n\n"
                "**Please read both sets of rules carefully.**\n\n"
                "Click the buttons below to view each rules section.\n"
                "After reviewing, click **I AGREE** to continue."
            ),
            color=VerificationEmbedBuilder.COLOR_INFO
        )
        embed.add_field(name=f"📋 {config.gang_name} Rules",
                        value=f"Click to view {config.gang_name}'s gang rules and expectations.", inline=False)
        embed.add_field(name="📋 Server Rules",
                        value="Click to view the community server rules.", inline=False)
        return embed

    @staticmethod
    def rules_display(title: str, rules: str, server_type: str = "GNG") -> discord.Embed:
        color = VerificationEmbedBuilder.COLOR_GANG if server_type == "GANG" else VerificationEmbedBuilder.COLOR_INFO
        emoji = "🟠" if server_type == "GANG" else "🔵"
        embed = VerificationEmbedBuilder.create_base_embed(
            title=f"{emoji} **{title}**",
            description=rules[:4096] if rules else "No rules available. Contact staff.",
            color=color, show_brand=False
        )
        embed.set_footer(text=f"{server_type} Rules • Scroll to read all rules")
        return embed

    @staticmethod
    def question_screen(question_number: int, total_questions: int, question: str,
                        label: str, hint: str = None, is_agreement: bool = False) -> discord.Embed:
        progress = int((question_number / total_questions) * 100)
        progress_bar = "█" * (progress // 10) + "░" * (10 - progress // 10)
        embed = VerificationEmbedBuilder.create_base_embed(
            title=f"❓ **Question {question_number} of {total_questions}**",
            description=(
                f"```ansi\n[2;34m{progress_bar}[0m {progress}% Complete\n```\n\n"
                f"**{label}**\n\n{question}"
            ),
            color=VerificationEmbedBuilder.COLOR_PRIMARY
        )
        if hint:
            embed.add_field(name="💡 Hint", value=hint, inline=False)
        if is_agreement:
            embed.add_field(name="⚠️ Important",
                            value="Type **'I agree'** to accept the terms and continue.", inline=False)
        return embed

    @staticmethod
    def confirmation_screen(responses: Dict[str, str]) -> discord.Embed:
        embed = VerificationEmbedBuilder.create_base_embed(
            title="📋 **REVIEW YOUR APPLICATION**",
            description=(
                "Please review your responses below before submitting.\n\n"
                "If everything looks correct, click **Submit Application**.\n"
                "If you need to make changes, click **Cancel** and restart."
            ),
            color=VerificationEmbedBuilder.COLOR_WARNING
        )
        for label, response in responses.items():
            if response:
                truncated = response[:200] + "..." if len(response) > 200 else response
                embed.add_field(name=f"• {label}", value=truncated, inline=False)
        return embed

    @staticmethod
    def submission_complete() -> discord.Embed:
        embed = VerificationEmbedBuilder.create_base_embed(
            title="🎉 **APPLICATION SUBMITTED!**",
            description=(
                "Your application has been successfully submitted for review!\n\n"
                "**What happens next?**\n"
                "• Staff will review your application\n"
                "• You'll receive a DM with the decision\n"
                "• This usually takes 1-24 hours\n\n"
                "Thank you for your patience! 🙏"
            ),
            color=VerificationEmbedBuilder.COLOR_SUCCESS
        )
        embed.add_field(name="💡 While You Wait",
                        value="Feel free to explore the server and chat with members!", inline=False)
        return embed

    @staticmethod
    def staff_submission(user: discord.User, responses: Dict[str, str], account_info: Dict) -> discord.Embed:
        embed = discord.Embed(
            title="📥 **NEW VERIFICATION SUBMISSION**",
            description=(
                f"**Applicant:** {user.mention} (`{user.name}`)\n"
                f"**User ID:** `{user.id}`"
            ),
            color=VerificationEmbedBuilder.COLOR_PRIMARY,
            timestamp=datetime.now(timezone.utc)
        )
        embed.set_thumbnail(url=user.avatar.url if user.avatar else user.default_avatar.url)
        embed.add_field(name="📊 Account Information", value=(
            f"**Account Age:** {account_info.get('account_age', 'Unknown')} days\n"
            f"**Joined Server:** {account_info.get('joined_age', 'Unknown')} days ago\n"
            f"**Created:** {account_info.get('created_at', 'Unknown')}"
        ), inline=True)
        security_flags = account_info.get('security_flags', [])
        if security_flags:
            embed.add_field(name="⚠️ Security Flags",
                            value="\n".join(f"• {flag}" for flag in security_flags), inline=True)
        else:
            embed.add_field(name="✅ Security Check", value="No security flags detected", inline=True)
        embed.add_field(name="─" * 30, value="", inline=False)
        for label, response in responses.items():
            if response:
                if label == "Invitation Proof" and response.startswith("http"):
                    embed.add_field(name=f"📸 {label}", value="📎 **[View Image]**", inline=False)
                    embed.set_image(url=response)
                else:
                    truncated = response[:300] + "..." if len(response) > 300 else response
                    embed.add_field(name=f"• {label}", value=truncated, inline=False)
        embed.set_footer(text=brand_text(f"Submitted at {datetime.now().strftime('%Y-%m-%d %H:%M:%S')} • GNG Staff Review"))
        return embed

    @staticmethod
    def decision_accepted(staff: discord.User, nickname: str = None) -> discord.Embed:
        embed = VerificationEmbedBuilder.create_base_embed(
            title="🎉 **WELCOME TO GNG!**",
            description=(
                "Congratulations! Your application has been **ACCEPTED**!\n\n"
                f"**Reviewed by:** {staff.mention}\n"
                f"{f'**Nickname:** `{nickname}`' if nickname else ''}"
            ),
            color=VerificationEmbedBuilder.COLOR_SUCCESS
        )
        embed.add_field(name="🎯 What's Next?", value=(
            "• You now have the **Member** role\n"
            "• Check out our channels and chat\n"
            "• Recruit new members for GNG!\n"
            "• Respect all members and allies\n\n"
            "**GNG × ALLIES ON TOP!** 🔥"
        ), inline=False)
        return embed

    @staticmethod
    def decision_declined(staff: discord.User, reason: str) -> discord.Embed:
        embed = VerificationEmbedBuilder.create_base_embed(
            title="❌ **APPLICATION DECLINED**",
            description=(
                "We're sorry, but your application has been declined.\n\n"
                f"**Reviewed by:** {staff.mention}\n"
                f"**Reason:** {reason}"
            ),
            color=VerificationEmbedBuilder.COLOR_ERROR
        )
        embed.add_field(name="📋 What Now?", value=(
            "• You may reapply after **24 hours**\n"
            "• Reapplying sooner may result in mod action\n"
            "• Contact staff if you have questions"
        ), inline=False)
        return embed

    @staticmethod
    def info_request(question: str, staff: discord.User) -> discord.Embed:
        embed = VerificationEmbedBuilder.create_base_embed(
            title="📝 **ADDITIONAL INFORMATION REQUESTED**",
            description=(
                f"Staff member {staff.mention} needs more information about your application.\n\n"
                f"**Question:**\n{question}"
            ),
            color=VerificationEmbedBuilder.COLOR_WARNING
        )
        embed.add_field(name="⏱️ Time Limit", value="Please respond within **5 minutes**", inline=False)
        return embed

    @staticmethod
    def timeout() -> discord.Embed:
        return VerificationEmbedBuilder.create_base_embed(
            title="⏰ **SESSION EXPIRED**",
            description=(
                "Your verification session has timed out due to inactivity.\n\n"
                "To start a new verification, type `!verify` in the verification channel."
            ),
            color=VerificationEmbedBuilder.COLOR_ERROR
        )


# --- PAGINATED VIEW (V2 Enhancement) ---
class PaginatedView(View):
    def __init__(self, user_id: int, items: List[str], title: str, color: discord.Color, items_per_page: int = 5, timeout: float = 180):
        super().__init__(timeout=timeout)
        self.user_id = user_id
        self.items = items
        self.title = title
        self.color = color
        self.items_per_page = items_per_page
        self.current_page = 0
        self.total_pages = max(1, (len(items) + items_per_page - 1) // items_per_page)
        self._update_buttons()
    
    def _update_buttons(self) -> None:
        self.previous_button.disabled = self.current_page == 0
        self.next_button.disabled = self.current_page >= self.total_pages - 1
    
    def _create_embed(self) -> discord.Embed:
        start = self.current_page * self.items_per_page
        end = start + self.items_per_page
        page_items = self.items[start:end]
        
        description = "\n\n".join(str(item) for item in page_items)
        
        embed = discord.Embed(title=self.title, description=description or "No items to display.", color=self.color)
        embed.set_footer(text=f"Page {self.current_page + 1}/{self.total_pages}")
        embed.timestamp = datetime.now(timezone.utc)
        
        return embed
    
    @discord.ui.button(label="◀️ Previous", style=discord.ButtonStyle.secondary)
    async def previous_button(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your menu!", ephemeral=True)
            return
        if self.current_page > 0:
            self.current_page -= 1
            self._update_buttons()
            await interaction.response.edit_message(embed=self._create_embed(), view=self)
        else:
            await interaction.response.defer()
    
    @discord.ui.button(label="▶️ Next", style=discord.ButtonStyle.secondary)
    async def next_button(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your menu!", ephemeral=True)
            return
        if self.current_page < self.total_pages - 1:
            self.current_page += 1
            self._update_buttons()
            await interaction.response.edit_message(embed=self._create_embed(), view=self)
        else:
            await interaction.response.defer()


# --- RULES VIEWS ---
class PaginatedRulesView(View):
    def __init__(self, user_id: int, title: str, emoji: str, color: discord.Color, rules_pages: List[str], timeout: float = 180):
        super().__init__(timeout=timeout)
        self.user_id = user_id
        self.title = title
        self.emoji = emoji
        self.color = color
        self.rules_pages = rules_pages
        self.current_page = 0
        self._update_buttons()
    
    def _update_buttons(self) -> None:
        self.previous_button.disabled = self.current_page == 0
        self.next_button.disabled = self.current_page >= len(self.rules_pages) - 1
    
    def _create_embed(self) -> discord.Embed:
        embed = discord.Embed(title=f"{self.emoji} {self.title}", description=self.rules_pages[self.current_page], color=self.color)
        embed.set_footer(text=f"Page {self.current_page + 1}/{len(self.rules_pages)}  GNG Verification System")
        embed.timestamp = datetime.now(timezone.utc)
        return embed
    
    @discord.ui.button(label="Previous", style=discord.ButtonStyle.secondary, custom_id="rules_prev_page")
    async def previous_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification!", ephemeral=True)
            return
        if self.current_page > 0:
            self.current_page -= 1
            self._update_buttons()
            await interaction.response.edit_message(embed=self._create_embed(), view=self)
        else:
            await interaction.response.defer()
    
    @discord.ui.button(label="Next", style=discord.ButtonStyle.secondary, custom_id="rules_next_page")
    async def next_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification!", ephemeral=True)
            return
        if self.current_page < len(self.rules_pages) - 1:
            self.current_page += 1
            self._update_buttons()
            await interaction.response.edit_message(embed=self._create_embed(), view=self)
        else:
            await interaction.response.defer()


def split_rules_into_pages(rules: str, max_chars: int = 3900) -> List[str]:
    if not rules:
        return ["No rules available. Contact staff."]
    if len(rules) <= max_chars:
        return [rules]
    
    pages: List[str] = []
    current_page = ""
    sections = rules.split('\n---\n')
    
    for section in sections:
        if len(section) > max_chars:
            paragraphs = section.split('\n\n')
            for para in paragraphs:
                if len(current_page) + len(para) + 2 > max_chars:
                    if current_page:
                        pages.append(current_page.strip())
                    current_page = para + "\n\n"
                else:
                    current_page += para + "\n\n"
        else:
            if len(current_page) + len(section) + 10 > max_chars:
                if current_page:
                    pages.append(current_page.strip())
                current_page = section + "\n\n---\n\n"
            else:
                current_page += section + "\n\n---\n\n"
    
    if current_page.strip():
        pages.append(current_page.strip())
    
    return pages if pages else [rules[:max_chars]]


class RulesButtonView(View):
    def __init__(self, user_id: int, timeout: float = 300):
        super().__init__(timeout=timeout)
        self.user_id = user_id
        self.rules_viewed = {'gang': False, 'server': False}
    
    def _create_rules_embed(self, title: str, rules: str, emoji: str, color: discord.Color, page_num: int = 1, total_pages: int = 1) -> discord.Embed:
        embed = discord.Embed(title=title, description=rules[:4096] if rules else "No rules available. Contact staff.", color=color)
        if total_pages > 1:
            embed.set_footer(text=f"Page {page_num}/{total_pages}  GNG Verification System")
        else:
            embed.set_footer(text="GNG Verification System")
        embed.timestamp = datetime.now(timezone.utc)
        return embed
    
    @discord.ui.button(label="GNG Rules", style=discord.ButtonStyle.primary, custom_id="gang_rules_button")
    async def gang_rules_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification!", ephemeral=True)
            return
        
        await interaction.response.defer(thinking=True)
        rules = await self._fetch_gang_rules()
        pages = split_rules_into_pages(rules)
        
        button.style = discord.ButtonStyle.success
        button.label = "GNG Rules Viewed"
        self.rules_viewed['gang'] = True
        await interaction.message.edit(view=self)
        
        if len(pages) > 1:
            paginated_view = PaginatedRulesView(self.user_id, "GNG In-Game Rules", "", discord.Color.red(), pages)
            await interaction.followup.send(embed=paginated_view._create_embed(), view=paginated_view, ephemeral=True)
        else:
            embed = self._create_rules_embed("GNG In-Game Rules", rules, "", discord.Color.red())
            await interaction.followup.send(embed=embed, ephemeral=True)
    
    @discord.ui.button(label="Server Rules", style=discord.ButtonStyle.secondary, custom_id="server_rules_button")
    async def server_rules_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification!", ephemeral=True)
            return
        
        await interaction.response.defer(thinking=True)
        rules = await self._fetch_server_rules()
        pages = split_rules_into_pages(rules)
        
        button.style = discord.ButtonStyle.success
        button.label = "Server Rules Viewed"
        self.rules_viewed['server'] = True
        await interaction.message.edit(view=self)
        
        if len(pages) > 1:
            paginated_view = PaginatedRulesView(self.user_id, "Server Game & Server Rules", "", discord.Color.blue(), pages)
            await interaction.followup.send(embed=paginated_view._create_embed(), view=paginated_view, ephemeral=True)
        else:
            embed = self._create_rules_embed("Server Game & Server Rule", rules, "", discord.Color.blue())
            await interaction.followup.send(embed=embed, ephemeral=True)
    
    async def _fetch_gang_rules(self) -> str:
        try:
            channel = bot.get_channel(config.servers.gang_rules_channel)
            if channel:
                try:
                    message = await channel.fetch_message(config.servers.gang_rules_message)
                    rules = message.content
                    rules_cache['gang_rules'] = rules
                    rules_cache['gang_last_updated'] = datetime.now(timezone.utc).isoformat()
                    save_rules_cache()
                    return rules
                except discord.NotFound:
                    logging.warning("[Rules] GNG rules message not found, using cache")
                except discord.Forbidden:
                    logging.warning("[Rules] No permission to fetch GNG rules, using cache")
        except Exception as e:
            logging.error(f"[Rules] Error fetching GNG rules: {e}")
        
        if rules_cache['gang_rules']:
            return rules_cache['gang_rules']
        return "Unable to fetch rules. Please contact a staff member."
    
    async def _fetch_server_rules(self) -> str:
        try:
            server_guild = bot.get_guild(config.servers.server_server_id)
            if server_guild:
                channel = server_guild.get_channel(config.servers.server_rules_channel)
                if channel:
                    try:
                        message = await channel.fetch_message(config.servers.server_rules_message)
                        rules = message.content
                        rules_cache['server_rules'] = rules
                        rules_cache['server_last_updated'] = datetime.now(timezone.utc).isoformat()
                        save_rules_cache()
                        return rules
                    except discord.NotFound:
                        logging.warning("[Rules] Server rules message not found, using cache")
                    except discord.Forbidden:
                        logging.warning("[Rules] No permission to fetch Server rules, using cache")
            else:
                logging.warning("[Rules] Bot not in Server server, using cache")
        except Exception as e:
            logging.error(f"[Rules] Error fetching Server rules: {e}")
        
        if rules_cache['server_rules']:
            return rules_cache['server_rules']
        return "Unable to fetch Server rules. The bot may not be in the Server server.\nPlease contact a staff member or check the Server server directly."


# --- VERIFICATION VIEWS ---
class DeclineReasonModal(discord.ui.Modal, title="Decline Application"):
    reason_input: discord.ui.TextInput
    
    def __init__(self, user_id: int, user_name: str, original_embed: discord.Embed, original_view: 'VerificationButtonsView'):
        super().__init__()
        self.user_id = user_id
        self.user_name = user_name
        self.original_embed = original_embed
        self.original_view = original_view
        
        self.reason_input = discord.ui.TextInput(
            label="Reason for Decline",
            placeholder="Please provide a reason for declining this application...",
            style=discord.TextStyle.paragraph,
            min_length=10,
            max_length=1000,
            required=True
        )
        self.add_item(self.reason_input)
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        reason = self.reason_input.value
        try:
            user = interaction.guild.get_member(self.user_id)
            
            self.original_embed.color = discord.Color.red()
            self.original_embed.title = f"DECLINED: {self.original_embed.title.replace('Verification Submission from ', '')}"
            self.original_embed.add_field(name="Decision", value=f"**DECLINED** by {interaction.user.mention}\n{datetime.now().strftime('%Y-%m-%d %H:%M:%S')}", inline=False)
            self.original_embed.add_field(name="Reason", value=reason, inline=False)
            
            for child in self.original_view.children:
                child.disabled = True
            
            await interaction.message.edit(embed=self.original_embed, view=self.original_view)
            await interaction.response.send_message(f"Application for **{self.user_name}** has been declined.", ephemeral=True)
            
            await self._notify_declined_user(user, reason)
            logging.info(f"Application declined for {self.user_name} by {interaction.user} - Reason: {reason}")
        except Exception as e:
            logging.error(f"Error in decline modal submit: {str(e)}")
            await interaction.response.send_message(f"An error occurred: {str(e)}", ephemeral=True)
    
    async def _notify_declined_user(self, user: Optional[discord.Member], reason: str) -> None:
        if not user:
            return
        try:
            decline_dm = discord.Embed(title="Application Declined", description="Your GNG application has been declined.", color=discord.Color.red())
            decline_dm.add_field(name="Reason", value=reason, inline=False)
            decline_dm.add_field(name="What Now?", value="You may reapply tomorrow, Reapplying today will likely result in Mod action.", inline=False)
            decline_dm.set_footer(text="GNG Verification Team")
            await user.send(embed=decline_dm)
        except discord.Forbidden:
            welcome_channel = bot.get_channel(config.channels.welcome)
            if welcome_channel:
                await welcome_channel.send(f"{user.mention} Your application has been declined. **Reason:** {reason}\nContact staff for more information.")


class RequestMoreInfoModal(discord.ui.Modal, title="Request More Information"):
    question_input: discord.ui.TextInput
    
    def __init__(self, user_id: int, user_name: str, original_embed: discord.Embed, original_view: 'VerificationButtonsView'):
        super().__init__()
        self.user_id = user_id
        self.user_name = user_name
        self.original_embed = original_embed
        self.original_view = original_view
        
        self.question_input = discord.ui.TextInput(
            label="Question for Applicant",
            placeholder="What additional information do you need from this applicant?",
            style=discord.TextStyle.paragraph,
            min_length=10,
            max_length=1000,
            required=True
        )
        self.add_item(self.question_input)
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        question = self.question_input.value
        try:
            user = interaction.guild.get_member(self.user_id)
            if not user:
                await interaction.response.send_message("User is no longer in the server.", ephemeral=True)
                return
            
            process_manager.add_verification(self.user_id)
            
            self.original_embed.add_field(name="Info Requested", value=f"**Question:** {question}\n**By:** {interaction.user.mention}\nWaiting for response...", inline=False)
            self.original_embed.color = discord.Color.orange()
            
            for child in self.original_view.children:
                child.disabled = True
            
            await interaction.message.edit(embed=self.original_embed, view=self.original_view)
            await interaction.response.send_message(f"Question sent to **{self.user_name}**. Waiting for their response...", ephemeral=True)
            
            response_received = await self._send_question_to_applicant(user, question, interaction)
            
            if not response_received:
                for child in self.original_view.children:
                    child.disabled = False
                self.original_embed.color = discord.Color.blurple()
                self.original_embed.add_field(name="No Response", value="The applicant did not respond in time.", inline=False)
                await interaction.message.edit(embed=self.original_embed, view=self.original_view)
            
            process_manager.remove_verification(self.user_id)
        except Exception as e:
            logging.error(f"Error in request more info modal: {str(e)}")
            process_manager.remove_verification(self.user_id)
            await interaction.response.send_message(f"An error occurred: {str(e)}", ephemeral=True)
    
    async def _send_question_to_applicant(self, user: discord.Member, question: str, staff_interaction: discord.Interaction) -> bool:
        try:
            dm_embed = discord.Embed(title="Additional Information Requested", description="The GNG staff needs more information about your application.", color=discord.Color.orange())
            dm_embed.add_field(name="Question", value=question, inline=False)
            dm_embed.add_field(name="Instructions", value="Please reply to this message with your answer within **5 minutes**.", inline=False)
            dm_embed.set_footer(text="GNG Verification Team")
            await user.send(embed=dm_embed)
            
            try:
                response_msg = await bot.wait_for('message', check=lambda m: m.author.id == self.user_id and m.channel.type == discord.ChannelType.private, timeout=300)
                
                response_text = response_msg.content
                if response_msg.attachments:
                    response_text += f"\n\nAttachment: {response_msg.attachments[0].url}"
                
                self.original_embed.add_field(name=f"Applicant Response", value=f"**Answer:** {response_text[:1000]}", inline=False)
                self.original_embed.color = discord.Color.blurple()
                
                for i, field in enumerate(self.original_embed.fields):
                    if field.name == "Info Requested":
                        self.original_embed.set_field_at(i, name="Info Requested", value=f"**Question:** {question}\n**By:** {staff_interaction.user.mention}\nResponse received!", inline=False)
                        break
                
                for child in self.original_view.children:
                    child.disabled = False
                
                channel = bot.get_channel(config.channels.verification_submission)
                if channel:
                    try:
                        message = await channel.fetch_message(staff_interaction.message.id)
                        await message.edit(embed=self.original_embed, view=self.original_view)
                        await channel.send(f"**{user.display_name}** has responded to the info request!", delete_after=30)
                    except discord.NotFound:
                        pass
                
                confirm_embed = discord.Embed(title="Response Received", description="Thank you! Your response has been sent to the staff for review.", color=discord.Color.green())
                await user.send(embed=confirm_embed)
                return True
                
            except asyncio.TimeoutError:
                timeout_embed = discord.Embed(title="Time Expired", description="You did not respond in time. Please contact staff directly.", color=discord.Color.red())
                try:
                    await user.send(embed=timeout_embed)
                except discord.Forbidden:
                    pass
                return False
                
        except discord.Forbidden:
            await staff_interaction.followup.send(f"Could not DM {user.mention}. They may have DMs disabled.", ephemeral=True)
            return False
        except Exception as e:
            logging.error(f"Error sending question to applicant: {str(e)}")
            return False


class VerificationButtonsView(View):
    def __init__(self):
        super().__init__(timeout=None)
    
    @discord.ui.button(label="Accept Application", style=discord.ButtonStyle.success, custom_id="accept_button")
    async def accept_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        try:
            embed = interaction.message.embeds[0]
            footer_text = embed.footer.text
            
            if "User ID:" not in footer_text:
                await interaction.response.send_message("Could not find user ID in the application.", ephemeral=True)
                return
            
            user_id = int(footer_text.split("User ID: ")[1].split(" ")[0])
            user = interaction.guild.get_member(user_id)
            
            if not user:
                await interaction.response.send_message("User not found in the server.", ephemeral=True)
                return
            
            member_role = interaction.guild.get_role(config.roles.member)
            
            if member_role:
                await user.add_roles(member_role)
                embed.color = discord.Color.green()
                embed.title = f"ACCEPTED: {embed.title.replace('Verification Submission from ', '')}"
                embed.add_field(name="Decision", value=f"**ACCEPTED** by {interaction.user.mention}\n{datetime.now().strftime('%Y-%m-%d %H:%M:%S')}", inline=False)
                
                for child in self.children:
                    child.disabled = True
                
                await interaction.message.edit(embed=embed, view=self)

                # --- SET NICKNAME: GNG_GangName ---
                nickname_set = False
                try:
                    gang_name = None

                    for field in embed.fields:
                        if field.name == "Desired Gang Name":
                            gang_name = field.value.strip()

                    if gang_name:
                        new_nickname = gang_name[:32]
                        await user.edit(nick=new_nickname)
                        nickname_set = True
                        logging.info(f"Nickname set to '{new_nickname}' for {user}")
                    else:
                        logging.warning(f"Could not set nickname for {user}: gang_name={gang_name}")
                except discord.Forbidden:
                    logging.warning(f"Missing permissions to change nickname for {user}")
                except Exception as nick_err:
                    logging.error(f"Failed to set nickname for {user}: {nick_err}")
                # ----------------------------------------------------------

                nick_note = f"\nNickname set to **{new_nickname}**" if nickname_set else "\n⚠️ Could not auto-set nickname (missing permissions or data)."
                await interaction.response.send_message(
                    f"Application for {user.mention} has been accepted. Member role assigned.{nick_note}",
                    ephemeral=True
                )
                
                await self._notify_accepted_user(user)
                logging.info(f"Application accepted for {user} by {interaction.user}")
            else:
                await interaction.response.send_message("Could not find the member role to assign.", ephemeral=True)
        except Exception as e:
            logging.error(f"Error in accept button handler: {str(e)}")
            await interaction.response.send_message(f"An error occurred: {str(e)}", ephemeral=True)
    
    @discord.ui.button(label="Decline Application", style=discord.ButtonStyle.danger, custom_id="decline_button")
    async def decline_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        try:
            embed = interaction.message.embeds[0]
            footer_text = embed.footer.text
            
            if "User ID:" not in footer_text:
                await interaction.response.send_message("Could not find user ID in the application.", ephemeral=True)
                return
            
            user_id = int(footer_text.split("User ID: ")[1].split(" ")[0])
            user_name = embed.title.replace("Verification Submission from ", "")
            
            modal = DeclineReasonModal(user_id, user_name, embed, self)
            await interaction.response.send_modal(modal)
        except Exception as e:
            logging.error(f"Error in decline button handler: {str(e)}")
            await interaction.response.send_message(f"An error occurred: {str(e)}", ephemeral=True)
    
    @discord.ui.button(label="Request More Info", style=discord.ButtonStyle.secondary, custom_id="request_info_button")
    async def request_info_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        try:
            embed = interaction.message.embeds[0]
            footer_text = embed.footer.text
            
            if "User ID:" not in footer_text:
                await interaction.response.send_message("Could not find user ID in the application.", ephemeral=True)
                return
            
            user_id = int(footer_text.split("User ID: ")[1].split(" ")[0])
            user_name = embed.title.replace("Verification Submission from ", "")
            
            user = interaction.guild.get_member(user_id)
            if not user:
                await interaction.response.send_message("This user is no longer in the server.", ephemeral=True)
                return
            
            modal = RequestMoreInfoModal(user_id, user_name, embed, self)
            await interaction.response.send_modal(modal)
        except Exception as e:
            logging.error(f"Error in request info button handler: {str(e)}")
            await interaction.response.send_message(f"An error occurred: {str(e)}", ephemeral=True)
    
    async def _notify_accepted_user(self, user: discord.Member) -> None:
        try:
            accept_dm = discord.Embed(title="Application Accepted!", description="Welcome to GNG! Your application has been accepted.", color=discord.Color.green())
            accept_dm.add_field(name="What Now?", value="You now have the Member role. You're officially part of the GNG Blood.. Go on and Recruit new members, Chill with GNG Members and Allies, And just enjoy life man. Because that's what it's all about. GNG x ALLIES ON TOP!", inline=False)
            await user.send(embed=accept_dm)
        except discord.Forbidden:
            welcome_channel = bot.get_channel(config.channels.welcome)
            if welcome_channel:
                await welcome_channel.send(f"{user.mention} Your application has been accepted! Welcome to GNG!")


# =============================================================================
# IMPROVED VERIFICATION VIEWS - Button-based interaction components
# =============================================================================

class WelcomeView(View):
    """Initial welcome screen with Start and Cancel buttons."""

    def __init__(self, user_id: int, timeout: float = 300):
        super().__init__(timeout=timeout)
        self.user_id = user_id
        self.started = False

    @discord.ui.button(label="🚀 START VERIFICATION", style=discord.ButtonStyle.success,
                       custom_id="v2_start_verification")
    async def start_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification session!", ephemeral=True)
            return
        self.started = True
        self.stop()
        for child in self.children:
            child.disabled = True
        await interaction.response.edit_message(view=self)

    @discord.ui.button(label="❌ Cancel", style=discord.ButtonStyle.secondary,
                       custom_id="v2_cancel_verification")
    async def cancel_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification session!", ephemeral=True)
            return
        self.started = False
        self.stop()
        for child in self.children:
            child.disabled = True
        embed = VerificationEmbedBuilder.create_base_embed(
            title="❌ **VERIFICATION CANCELLED**",
            description="You've cancelled the verification process. Type `!verify` to start again.",
            color=VerificationEmbedBuilder.COLOR_ERROR
        )
        await interaction.response.edit_message(embed=embed, view=self)


class ImprovedRulesView(View):
    """Rules review with GNG/Server buttons and agreement confirmation."""

    def __init__(self, user_id: int, gang_rules: str, server_rules: str, timeout: float = 600):
        super().__init__(timeout=timeout)
        self.user_id = user_id
        self.gang_rules = gang_rules
        self.server_rules = server_rules
        self.viewed = {'gang': False, 'server': False}
        self.agreed = False

    def _update_buttons(self):
        if self.viewed['gang']:
            self.gang_button.style = discord.ButtonStyle.success
            self.gang_button.label = "✅ GNG Rules Viewed"
        if self.viewed['server']:
            self.server_button.style = discord.ButtonStyle.success
            self.server_button.label = "✅ Server Rules Viewed"
        if self.viewed['gang'] and self.viewed['server']:
            self.agree_button.disabled = False
            self.agree_button.style = discord.ButtonStyle.success

    @discord.ui.button(label="🟠 GNG Rules", style=discord.ButtonStyle.primary,
                       custom_id="v2_view_gang_rules", emoji="📜")
    async def gang_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification session!", ephemeral=True)
            return
        pages = split_rules_into_pages(self.gang_rules)
        if len(pages) > 1:
            view = PaginatedRulesView(self.user_id, "GNG In-Game Rules", "", discord.Color.red(), pages)
            await interaction.response.send_message(embed=view._create_embed(), view=view, ephemeral=True)
        else:
            embed = VerificationEmbedBuilder.rules_display("GNG In-Game Rules", self.gang_rules, "GNG")
            await interaction.response.send_message(embed=embed, ephemeral=True)
        self.viewed['gang'] = True
        self._update_buttons()
        await interaction.message.edit(view=self)

    @discord.ui.button(label="🔵 Server Rules", style=discord.ButtonStyle.primary,
                       custom_id="v2_view_server_rules", emoji="📜")
    async def server_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification session!", ephemeral=True)
            return
        pages = split_rules_into_pages(self.server_rules)
        if len(pages) > 1:
            view = PaginatedRulesView(self.user_id, "Server Server Rules", "", discord.Color.blue(), pages)
            await interaction.response.send_message(embed=view._create_embed(), view=view, ephemeral=True)
        else:
            embed = VerificationEmbedBuilder.rules_display("Server Server Rules", self.server_rules, "Server")
            await interaction.response.send_message(embed=embed, ephemeral=True)
        self.viewed['server'] = True
        self._update_buttons()
        await interaction.message.edit(view=self)

    @discord.ui.button(label="✓ I AGREE TO ALL RULES", style=discord.ButtonStyle.secondary,
                       custom_id="v2_agree_rules", disabled=True, row=1)
    async def agree_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification session!", ephemeral=True)
            return
        if not (self.viewed['gang'] and self.viewed['server']):
            await interaction.response.send_message("Please view both rules sections first!", ephemeral=True)
            return
        self.agreed = True
        self.stop()
        for child in self.children:
            child.disabled = True
        await interaction.response.edit_message(view=self)

    @discord.ui.button(label="❌ Cancel", style=discord.ButtonStyle.danger,
                       custom_id="v2_cancel_rules", row=1)
    async def cancel_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your verification session!", ephemeral=True)
            return
        self.agreed = False
        self.stop()




class ImprovedConfirmationView(View):
    """Final confirmation view with Submit and Cancel buttons."""

    def __init__(self, user_id: int, timeout: float = 120):
        super().__init__(timeout=timeout)
        self.user_id = user_id
        self.confirmed = False

    @discord.ui.button(label="✅ SUBMIT APPLICATION", style=discord.ButtonStyle.success,
                       custom_id="v2_confirm_submit")
    async def submit_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your session!", ephemeral=True)
            return
        self.confirmed = True
        self.stop()
        for child in self.children:
            child.disabled = True
        await interaction.response.edit_message(
            embed=VerificationEmbedBuilder.create_base_embed(
                title="⏳ **SUBMITTING...**",
                description="Please wait while we process your application...",
                color=VerificationEmbedBuilder.COLOR_INFO
            ),
            view=self
        )

    @discord.ui.button(label="❌ CANCEL", style=discord.ButtonStyle.danger,
                       custom_id="v2_confirm_cancel")
    async def cancel_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your session!", ephemeral=True)
            return
        self.confirmed = False
        self.stop()
        for child in self.children:
            child.disabled = True
        await interaction.response.edit_message(
            embed=VerificationEmbedBuilder.create_base_embed(
                title="❌ **CANCELLED**",
                description="Your application has been discarded. Type `!verify` to start again.",
                color=VerificationEmbedBuilder.COLOR_ERROR
            ),
            view=self
        )


# =============================================================================
# IMPROVED STAFF ACTION VIEW — replaces old VerificationButtonsView for new
# submissions. Old VerificationButtonsView is kept for backward compatibility.
# =============================================================================

class ImprovedStaffActionView(View):
    """Full-featured staff action view with Accept/Decline/Request Info/Note."""

    def __init__(self, user_id: int, user_name: str, submission_embed: discord.Embed):
        super().__init__(timeout=None)  # Persistent
        self.target_user_id = user_id
        self.user_name = user_name
        self.submission_embed = submission_embed

    def disable_all(self):
        for child in self.children:
            child.disabled = True

    async def _get_state_from_embed(self, interaction: discord.Interaction):
        """
        Extract user_id and name from the submission embed so staff actions
        survive bot restarts (persistent views are re-registered with dummy
        state on startup, so the real state must be recovered from the
        message embed itself).

        Robust to embed format drift: tries multiple parse strategies and
        logs a warning instead of failing silently when parsing fails.
        """
        embed = interaction.message.embeds[0] if interaction.message.embeds else self.submission_embed
        desc = embed.description or ""
        fields = {f.name: (f.value or "") for f in embed.fields}

        user_id = self.target_user_id
        user_name = self.user_name

        # ── User ID ───────────────────────────────────────────────────
        parsed_uid = None
        # Strategy 1: the canonical "**User ID:** `123`" token.
        if "**User ID:** `" in desc:
            try:
                parsed_uid = int(desc.split("**User ID:** `")[1].split("`")[0])
            except (ValueError, IndexError) as exc:
                logging.warning(f"[StaffActionView] Failed to parse user_id via primary pattern: {exc}")
        # Strategy 2: any "User ID" field/label followed by a snowflake.
        if parsed_uid is None:
            haystack = desc + "\n" + "\n".join(f"{k}: {v}" for k, v in fields.items())
            m = re.search(r"User\s*ID:?\s*`?(\d{15,20})`?", haystack, re.IGNORECASE)
            if m:
                parsed_uid = int(m.group(1))
        # Strategy 3: bare snowflake in a dedicated field named "User ID".
        if parsed_uid is None and "User ID" in fields:
            m = re.search(r"(\d{15,20})", fields["User ID"])
            if m:
                parsed_uid = int(m.group(1))
        if parsed_uid is not None:
            user_id = parsed_uid
        elif self.target_user_id == 0:
            logging.error(
                "[StaffActionView] Could not recover target user_id from embed "
                f"(message_id={getattr(interaction.message, 'id', '?')}). "
                "Staff action will be blocked to prevent operating on the wrong user."
            )

        # ── User name ─────────────────────────────────────────────────
        parsed_name = None
        if "**Applicant:**" in desc:
            try:
                # Canonical: **Applicant:** {mention} (`name`)
                parsed_name = desc.split("**Applicant:** ")[1].split("`")[1]
            except IndexError:
                # Fallback: strip Markdown/mentions from the rest of the line.
                try:
                    rest = desc.split("**Applicant:** ")[1].split("\n")[0]
                    cleaned = re.sub(r"<@!?\d+>", "", rest).replace("`", "").strip(" -*•")
                    parsed_name = cleaned or None
                except Exception:
                    parsed_name = None
        if parsed_name:
            user_name = parsed_name
        elif not self.user_name and parsed_name is None:
            logging.warning("[StaffActionView] Could not recover applicant name from embed.")

        return user_id, user_name, embed

    def _state_unrecoverable(self, user_id: int, interaction: discord.Interaction) -> bool:
        """Returns True if we could not recover the applicant's user_id."""
        if user_id and user_id != 0:
            return False
        # Block the action: opening a modal against user_id=0 would silently
        # accept/decline the wrong (non-existent) applicant.
        logging.error(
            f"[StaffActionView] Blocking staff action on message "
            f"{getattr(interaction.message, 'id', '?')} - applicant user_id "
            "could not be recovered from the embed."
        )
        return True

    @discord.ui.button(label="✅ ACCEPT", style=discord.ButtonStyle.success,
                       custom_id="improved_staff_accept", emoji="👍")
    async def accept_button(self, interaction: discord.Interaction, button: Button):
        user_id, user_name, embed = await self._get_state_from_embed(interaction)
        if self._state_unrecoverable(user_id, interaction):
            await interaction.response.send_message(
                "⚠️ This submission's applicant could not be identified "
                "(the embed format may have changed). Please process it manually.",
                ephemeral=True,
            )
            return
        await interaction.response.send_modal(
            ImprovedAcceptModal(user_id, user_name, embed, self)
        )

    @discord.ui.button(label="❌ DECLINE", style=discord.ButtonStyle.danger,
                       custom_id="improved_staff_decline", emoji="👎")
    async def decline_button(self, interaction: discord.Interaction, button: Button):
        user_id, user_name, embed = await self._get_state_from_embed(interaction)
        if self._state_unrecoverable(user_id, interaction):
            await interaction.response.send_message(
                "⚠️ This submission's applicant could not be identified "
                "(the embed format may have changed). Please process it manually.",
                ephemeral=True,
            )
            return
        await interaction.response.send_modal(
            ImprovedDeclineModal(user_id, user_name, embed, self)
        )

    @discord.ui.button(label="❓ REQUEST INFO", style=discord.ButtonStyle.secondary,
                       custom_id="improved_staff_request_info", emoji="💬")
    async def request_info_button(self, interaction: discord.Interaction, button: Button):
        user_id, user_name, embed = await self._get_state_from_embed(interaction)
        if self._state_unrecoverable(user_id, interaction):
            await interaction.response.send_message(
                "⚠️ This submission's applicant could not be identified "
                "(the embed format may have changed). Please process it manually.",
                ephemeral=True,
            )
            return
        await interaction.response.send_modal(
            ImprovedRequestInfoModal(user_id, user_name, embed, self)
        )

    @discord.ui.button(label="📝 ADD NOTE", style=discord.ButtonStyle.primary,
                       custom_id="improved_staff_note", emoji="📌", row=1)
    async def note_button(self, interaction: discord.Interaction, button: Button):
        user_id, user_name, embed = await self._get_state_from_embed(interaction)
        if self._state_unrecoverable(user_id, interaction):
            await interaction.response.send_message(
                "⚠️ This submission's applicant could not be identified "
                "(the embed format may have changed). Please process it manually.",
                ephemeral=True,
            )
            return
        await interaction.response.send_modal(
            ImprovedStaffNoteModal(user_id, embed, self)
        )


class ImprovedAcceptModal(Modal, title="✅ Accept Application"):
    gang_name = TextInput(
        label="Gang Name (optional — sets nickname)",
        placeholder="GNG_GangName",
        required=False,
        max_length=32
    )
    notes = TextInput(
        label="Staff Notes (internal)",
        style=discord.TextStyle.paragraph,
        required=False,
        max_length=500
    )

    def __init__(self, user_id: int, user_name: str, embed: discord.Embed, view: ImprovedStaffActionView):
        super().__init__()
        self.target_user_id = user_id
        self.user_name = user_name
        self.embed = embed
        self.staff_view = view

    async def on_submit(self, interaction: discord.Interaction):
        # ── 1. Update the submission embed ──────────────────────────────────
        self.embed.color = VerificationEmbedBuilder.COLOR_SUCCESS
        self.embed.title = f"✅ ACCEPTED: {self.user_name}"
        self.embed.add_field(
            name="════════ DECISION ════════",
            value=(
                f"**Status:** ✅ ACCEPTED\n"
                f"**By:** {interaction.user.mention}\n"
                f"**Time:** {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}"
            ),
            inline=False
        )
        if self.gang_name.value:
            self.embed.add_field(name="Assigned Gang Name", value=f"`{self.gang_name.value}`", inline=False)
        if self.notes.value:
            self.embed.add_field(name="📝 Staff Notes", value=self.notes.value, inline=False)

        self.staff_view.disable_all()
        await interaction.response.edit_message(embed=self.embed, view=self.staff_view)

        # ── 2. Assign member role ────────────────────────────────────────────
        user = interaction.guild.get_member(self.target_user_id)
        nickname_text = ""
        if user:
            member_role = interaction.guild.get_role(config.roles.member)
            if member_role:
                try:
                    await user.add_roles(member_role)
                except Exception as e:
                    logging.warning(f"[ImprovedAccept] Could not add member role: {e}")

            # ── 3. Set nickname to provided gang name only ────────────────────
            try:
                gang = self.gang_name.value.strip() if self.gang_name.value else None
                new_nick = gang[:32] if gang else None
                if new_nick:
                    await user.edit(nick=new_nick)
                    nickname_text = f"\nNickname set to **{new_nick}**"
                    logging.info(f"[ImprovedAccept] Nickname '{new_nick}' set for {user}")
            except discord.Forbidden:
                nickname_text = "\n⚠️ Could not set nickname (missing permissions)."
                logging.warning(f"[ImprovedAccept] Missing perms to set nickname for {user}")
            except Exception as e:
                nickname_text = "\n⚠️ Nickname not set (error)."
                logging.error(f"[ImprovedAccept] Nickname error: {e}")

            # ── 4. DM the applicant ──────────────────────────────────────────
            try:
                accept_embed = VerificationEmbedBuilder.decision_accepted(
                    interaction.user, self.gang_name.value or None
                )
                await user.send(embed=accept_embed)
            except discord.Forbidden:
                welcome_channel = bot.get_channel(config.channels.welcome)
                if welcome_channel:
                    await welcome_channel.send(
                        brand_text(f"{user.mention} Your application has been accepted! Welcome to GNG!"))

        await interaction.followup.send(
            f"✅ **{self.user_name}**'s application has been accepted!{nickname_text}",
            ephemeral=True
        )
        logging.info(f"[ImprovedAccept] {self.user_name} accepted by {interaction.user}")


class ImprovedDeclineModal(Modal, title="❌ Decline Application"):
    reason = TextInput(
        label="Reason for decline",
        style=discord.TextStyle.paragraph,
        placeholder="Please provide a clear reason...",
        required=True,
        min_length=10,
        max_length=1000
    )

    def __init__(self, user_id: int, user_name: str, embed: discord.Embed, view: ImprovedStaffActionView):
        super().__init__()
        self.target_user_id = user_id
        self.user_name = user_name
        self.embed = embed
        self.staff_view = view

    async def on_submit(self, interaction: discord.Interaction):
        self.embed.color = VerificationEmbedBuilder.COLOR_ERROR
        self.embed.title = f"❌ DECLINED: {self.user_name}"
        self.embed.add_field(
            name="════════ DECISION ════════",
            value=(
                f"**Status:** ❌ DECLINED\n"
                f"**By:** {interaction.user.mention}\n"
                f"**Time:** {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}"
            ),
            inline=False
        )
        self.embed.add_field(name="Reason", value=self.reason.value, inline=False)
        self.staff_view.disable_all()
        await interaction.response.edit_message(embed=self.embed, view=self.staff_view)

        # DM the applicant
        user = interaction.guild.get_member(self.target_user_id)
        if user:
            try:
                decline_embed = VerificationEmbedBuilder.decision_declined(interaction.user, self.reason.value)
                await user.send(embed=decline_embed)
            except discord.Forbidden:
                welcome_channel = bot.get_channel(config.channels.welcome)
                if welcome_channel:
                    await welcome_channel.send(
                    brand_text(f"{user.mention} Your application has been declined. **Reason:** {self.reason.value}"))

        await interaction.followup.send(
            f"❌ **{self.user_name}**'s application has been declined.", ephemeral=True
        )
        logging.info(f"[ImprovedDecline] {self.user_name} declined by {interaction.user}: {self.reason.value}")


class ImprovedRequestInfoModal(Modal, title="❓ Request Additional Info"):
    question = TextInput(
        label="Question for applicant",
        style=discord.TextStyle.paragraph,
        placeholder="What additional information do you need?",
        required=True,
        min_length=10,
        max_length=1000
    )

    def __init__(self, user_id: int, user_name: str, embed: discord.Embed, view: ImprovedStaffActionView):
        super().__init__()
        self.target_user_id = user_id
        self.user_name = user_name
        self.embed = embed
        self.staff_view = view

    async def on_submit(self, interaction: discord.Interaction):
        self.embed.color = VerificationEmbedBuilder.COLOR_WARNING
        self.embed.add_field(
            name="════════ INFO REQUESTED ════════",
            value=(
                f"**Question:** {self.question.value}\n"
                f"**By:** {interaction.user.mention}\n"
                "⏳ Waiting for response..."
            ),
            inline=False
        )
        await interaction.response.edit_message(embed=self.embed)

        # DM the applicant the question
        user = interaction.guild.get_member(self.target_user_id)
        if user:
            try:
                info_embed = VerificationEmbedBuilder.info_request(self.question.value, interaction.user)
                await user.send(embed=info_embed)
            except discord.Forbidden:
                pass

        await interaction.followup.send(
            f"❓ Question sent to **{self.user_name}**. Waiting for their response...", ephemeral=True
        )


class ImprovedStaffNoteModal(Modal, title="📝 Add Staff Note"):
    note = TextInput(
        label="Internal note",
        style=discord.TextStyle.paragraph,
        placeholder="This note is only visible to staff...",
        required=True,
        max_length=1000
    )

    def __init__(self, user_id: int, embed: discord.Embed, view: ImprovedStaffActionView):
        super().__init__()
        self.target_user_id = user_id
        self.embed = embed
        self.staff_view = view

    async def on_submit(self, interaction: discord.Interaction):
        self.embed.add_field(
            name=f"📝 Staff Note by {interaction.user.display_name}",
            value=self.note.value,
            inline=False
        )
        await interaction.response.edit_message(embed=self.embed)
        await interaction.followup.send("📝 Note added successfully!", ephemeral=True)


# =============================================================================
# IMPROVED VERIFICATION SYSTEM - Full flow using new UI components
# =============================================================================

class ImprovedVerificationSystem:
    """Drop-in improved verification flow using the new UI components."""

    def __init__(self, bot_instance, cfg, data_mgr):
        self.bot = bot_instance
        self.config = cfg
        self.data_manager = data_mgr

    async def start_verification(self, ctx) -> bool:
        user = ctx.author
        now = datetime.now(timezone.utc)
        created_at = user.created_at.replace(tzinfo=timezone.utc) if user.created_at.tzinfo is None else user.created_at

        if ows_get("enforce_account_age"):
            if (now - created_at).days < self.config.limits.min_account_age_days:
                await ctx.send(embed=VerificationEmbedBuilder.create_base_embed(
                    title="❌ **ACCOUNT TOO NEW**",
                    description=(
                        f"Your Discord account is too new to verify.\n\n"
                        f"**Account Age:** {(now - created_at).days} days\n"
                        f"**Required:** {self.config.limits.min_account_age_days} days\n\n"
                        "Please wait and try again later."
                    ),
                    color=VerificationEmbedBuilder.COLOR_ERROR
                ))
                return False

        member_role = ctx.guild.get_role(self.config.roles.member)
        tester_role = ctx.guild.get_role(self.config.roles.server_tester)
        is_tester = tester_role and tester_role in user.roles
        if member_role and member_role in user.roles and not is_tester:
            await ctx.send(embed=VerificationEmbedBuilder.create_base_embed(
                title="✅ **ALREADY VERIFIED**",
                description="You are already a verified member of GNG!",
                color=VerificationEmbedBuilder.COLOR_SUCCESS
            ))
            return False

        try:
            welcome_embed = VerificationEmbedBuilder.welcome_screen(user)
            welcome_view = WelcomeView(user.id, timeout=300)
            await user.send(embed=welcome_embed, view=welcome_view)
            mainchannel = self.bot.get_channel(self.config.channels.verification_main)
            if mainchannel:
                await mainchannel.send(f"📬 Check your DMs, {user.mention}!")

            await welcome_view.wait()
            if not welcome_view.started:
                return False

            process_manager.add_verification(user.id)

            responses = await self._collect_responses(user)
            if responses is None:
                process_manager.remove_verification(user.id)
                return False

            confirmed = await self._show_confirmation(user, responses)
            if not confirmed:
                process_manager.remove_verification(user.id)
                return False

            await self._submit_to_staff(ctx, user, responses)
            process_manager.remove_verification(user.id)
            return True

        except discord.Forbidden:
            mainchannel = self.bot.get_channel(self.config.channels.verification_main)
            if mainchannel:
                await mainchannel.send(
                    f"❌ {user.mention}, I couldn't send you a DM! "
                    "Please enable Direct Messages from server members and try again."
                )
            return False
        except Exception as e:
            process_manager.remove_verification(user.id)
            logging.error(f"[ImprovedVerification] Error for {user}: {e}")
            try:
                await user.send("An error occurred during verification. Please contact staff.")
            except discord.Forbidden:
                pass
            return False

    async def _collect_responses(self, user) -> Optional[Dict[str, str]]:
        responses: Dict[str, str] = {}
        total = len(VERIFICATION_QUESTIONS_V2)

        for idx, q_data in enumerate(VERIFICATION_QUESTIONS_V2, 1):
            label          = q_data["label"]
            question       = q_data["question"]
            hint           = q_data.get("hint")
            is_agreement   = q_data.get("is_agreement", False)
            skip_allowed   = q_data.get("skip_allowed", False)
            accepts_attach = q_data.get("accepts_attachment", False)

            # ── Rules agreement step ─────────────────────────────────────
            if is_agreement:
                agreed = await self._handle_agreement(user)
                if not agreed:
                    await user.send(embed=VerificationEmbedBuilder.create_base_embed(
                        title="❌ **VERIFICATION CANCELLED**",
                        description="You declined the rules. Type `!verify` to start again.",
                        color=VerificationEmbedBuilder.COLOR_ERROR
                    ))
                    return None
                responses[label] = "✅ Agreed to Terms & Rules"
                continue

            # ── Conditional question handling ────────────────────────────
            depends_on = q_data.get("depends_on_label")
            if depends_on:
                previous_answer = responses.get(depends_on, "").strip()
                if not previous_answer or previous_answer.lower() == "[skipped]":
                    continue
                # Word-boundary-safe negative check. The old substring check
                # matched 'na' inside 'natural'/'banana', 'not' inside 'noted',
                # etc. and skipped questions that should have been asked.
                if _is_negative_answer(previous_answer):
                    continue

            # ── Regular question ─────────────────────────────────────────
            embed = VerificationEmbedBuilder.question_screen(
            idx, total, brand_text(question), label, brand_text(hint) if hint else hint)
            embed.add_field(name="⏭️ Skip Option",
                                value="Type `skip` to skip this question", inline=False)
            await user.send(embed=embed)

            try:
                msg = await self.bot.wait_for(
                    'message',
                    check=lambda m: m.author == user and m.channel.type == discord.ChannelType.private,
                    timeout=self.config.timing.verification_timeout_seconds
                )

                content = msg.content.strip()

                if content.lower() == 'cancel':
                    await user.send(embed=VerificationEmbedBuilder.create_base_embed(
                        title="❌ **VERIFICATION CANCELLED**",
                        description="Type `!verify` to start again.",
                        color=VerificationEmbedBuilder.COLOR_ERROR
                    ))
                    return None

                if skip_allowed and content.lower() == 'skip':
                    responses[label] = "[Skipped]"
                    continue

                if accepts_attach and msg.attachments:
                    responses[label] = msg.attachments[0].url
                else:
                    responses[label] = content

            except asyncio.TimeoutError:
                await user.send(embed=VerificationEmbedBuilder.timeout())
                return None

        return responses

    async def _handle_agreement(self, user) -> bool:
        cached = self.data_manager.load_rules_cache()
        gang_rules  = cached.get('gang_rules', "Unable to fetch GNG rules. Contact staff.")
        server_rules = cached.get('server_rules', "Unable to fetch Server rules. Contact staff.")

        embed = VerificationEmbedBuilder.rules_intro()
        view  = ImprovedRulesView(user.id, gang_rules, server_rules, timeout=600)
        await user.send(embed=embed, view=view)
        await view.wait()
        return view.agreed

    async def _show_confirmation(self, user, responses: Dict[str, str]) -> bool:
        embed = VerificationEmbedBuilder.confirmation_screen(responses)
        view  = ImprovedConfirmationView(user.id, timeout=120)
        await user.send(embed=embed, view=view)
        await view.wait()
        return view.confirmed

    async def _submit_to_staff(self, ctx, user, responses: Dict[str, str]):
        now = datetime.now(timezone.utc)
        created_at = user.created_at.replace(tzinfo=timezone.utc) if user.created_at.tzinfo is None else user.created_at
        joined_at  = (user.joined_at.replace(tzinfo=timezone.utc)
                      if user.joined_at and user.joined_at.tzinfo is None else user.joined_at)

        account_info = {
            'account_age':    (now - created_at).days,
            'joined_age':     (now - joined_at).days if joined_at else 'Unknown',
            'created_at':     created_at.strftime('%Y-%m-%d'),
            'security_flags': self._get_security_flags(user)
        }

        embed = VerificationEmbedBuilder.staff_submission(user, responses, account_info)
        view  = ImprovedStaffActionView(user.id, user.display_name, embed)

        channel = self.bot.get_channel(self.config.channels.verification_submission)
        if channel:
            await channel.send(
                f"<@&{self.config.roles.verification_ping}> 📥 **New Application!**",
                embed=embed, view=view
            )

        await user.send(embed=VerificationEmbedBuilder.submission_complete())

    @staticmethod
    def _get_security_flags(user) -> List[str]:
        flags = []
        if not user.avatar:
            flags.append("No profile picture")
        if len(user.display_name) < 3:
            flags.append("Very short username")
        if user.display_name.isdigit():
            flags.append("Username is all numbers")
        now = datetime.now(timezone.utc)
        created_at = user.created_at.replace(tzinfo=timezone.utc) if user.created_at.tzinfo is None else user.created_at
        if (now - created_at).days < 30:
            flags.append("Account less than 30 days old")
        return flags


# --- TICKET TOOL SYSTEM (Full Ticket Tool Clone - All Features Free) ---

class TicketToolSystem:
    """Main ticket tool system manager - handles all ticket operations."""
    
    def __init__(self, data_mgr: DataManager, bot_instance: commands.Bot):
        self.data_manager = data_mgr
        self.bot = bot_instance
        # Per-(guild, user) locks to prevent the ticket-limit race condition:
        # two near-simultaneous "Create Ticket" clicks could both pass the
        # open-count check before either row is written. Serializing per user
        # closes that window without blocking unrelated users.
        self._creation_locks: Dict[Tuple[int, int], asyncio.Lock] = {}
        self._creation_locks_guard = asyncio.Lock()
    
    async def _get_creation_lock(self, guild_id: int, user_id: int) -> asyncio.Lock:
        async with self._creation_locks_guard:
            key = (guild_id, user_id)
            lock = self._creation_locks.get(key)
            if lock is None:
                lock = asyncio.Lock()
                self._creation_locks[key] = lock
            return lock
    
    async def create_ticket(
        self, 
        guild: discord.Guild, 
        user: discord.Member,
        panel: Dict,
        subject: str = None,
        answers: Dict = None
    ) -> Tuple[Optional[discord.TextChannel], str]:
        """Create a new ticket channel.

        Transactional ordering (fixes orphan-channel / orphan-row bugs):
          1. Insert the ticket row with status='pending' FIRST (no channel yet).
          2. Create the Discord channel.
             - On failure: delete the pending row so DB and Discord stay
               consistent.
          3. Update the row with channel_id and status='open'.
          4. Persist any panel answers. If answer persistence fails, the
             ticket itself still exists (logged, not fatal).

        A per-user asyncio.Lock guards the limit check so two concurrent
        create requests can't both pass it.
        """
        import uuid
        ticket_id = str(uuid.uuid4())[:8]
        now_iso = datetime.now(timezone.utc).isoformat()

        # Owner Settings gate: if the Tickets System is disabled, refuse all
        # new ticket creation. Management commands (close/claim/transcript) are
        # intentionally left available so staff can wind down existing tickets,
        # but no NEW tickets can be opened while the system is disabled.
        if not ows_get("enable_tickets"):
            return None, "The ticket system is currently disabled by the server owner. Please try again later."

        # Check if user is blacklisted
        blacklisted, reason = self.data_manager.is_user_blacklisted(guild.id, user.id)
        if blacklisted:
            return None, f"You are blacklisted from creating tickets. Reason: {reason}"
        
        # Serialize ticket creation per user to close the limit-check race window.
        # The lock is held across the ENTIRE create (limit-check → pending insert
        # → Discord channel creation → open status update) so a second concurrent
        # request from the same user cannot slip in between the limit check and
        # the channel creation. The limit check also now counts BOTH 'pending'
        # and 'open' tickets, so the in-flight 'pending' row is visible to it.
        creation_lock = await self._get_creation_lock(guild.id, user.id)
        async with creation_lock:
            settings = self.data_manager.load_ticket_settings(guild.id)
            max_tickets = settings.get('max_tickets_per_user', 3) if settings else 3
            active_count = self.data_manager.count_active_tickets_by_creator(user.id, guild.id)
            if active_count >= max_tickets:
                return None, f"You already have {active_count} active ticket(s). Close one first."
        
            # Get panel settings
            category_id = panel.get('category_id') or (settings.get('category_id') if settings else None)
            support_role_id = panel.get('support_role_id') or (settings.get('support_role_id') if settings else None)
            
            # Channel name is made unique by appending the short ticket id,
            # so multiple tickets from the same user no longer collide.
            safe_name = ''.join(c if c.isalnum() or c == '-' else '-' for c in user.display_name.lower())[:40]
            channel_name = f"ticket-{safe_name}-{ticket_id}"[:90]
            
            # Setup permissions
            overwrites = {
                guild.default_role: discord.PermissionOverwrite(view_channel=False),
                user: discord.PermissionOverwrite(view_channel=True, send_messages=True, read_message_history=True, attach_files=True),
                guild.me: discord.PermissionOverwrite(view_channel=True, send_messages=True, manage_channels=True, read_message_history=True)
            }
            
            # Add support role
            if support_role_id:
                support_role = guild.get_role(support_role_id)
                if support_role:
                    overwrites[support_role] = discord.PermissionOverwrite(
                        view_channel=True, send_messages=True, read_message_history=True, attach_files=True
                    )
            
            # Get category
            category = guild.get_channel(category_id) if category_id else None
            
            # ---- STEP 1: persist a 'pending' ticket row BEFORE creating the channel ----
            ticket_data = {
                'ticket_id': ticket_id,
                'guild_id': guild.id,
                'channel_id': None,           # filled in after channel creation
                'panel_id': panel.get('panel_id'),
                'creator_id': user.id,
                'category': panel.get('name', 'General'),
                'subject': subject,
                'status': 'pending',
                'created_at': now_iso
            }
            await self.data_manager.async_save_ticket(ticket_data)

            # ---- STEP 2: create the Discord channel (INSIDE the user lock) ----
            # Holding the lock here is the key race fix: the 'pending' row is
            # already persisted, so a concurrent second request from the same
            # user would see it via count_active_tickets_by_creator and be
            # rejected at the limit check above before reaching this point.
            try:
                channel = await guild.create_text_channel(
                    channel_name,
                    category=category,
                    overwrites=overwrites,
                    topic=f"Ticket {ticket_id} - {user}"
                )
            except Exception as e:
                # Channel creation failed: roll back the pending DB row so we
                # don't leave an orphan ticket with no channel.
                logging.error(f"[TicketTool] Failed to create channel: {e}")
                try:
                    await self.data_manager.async_save_ticket({
                        **ticket_data, 'status': 'failed', 'close_reason': f'Channel creation failed: {e}',
                        'closed_at': datetime.now(timezone.utc).isoformat(),
                    })
                except Exception:
                    pass
                return None, f"Failed to create ticket channel: {e}"

            # ---- STEP 3: update the row with the real channel_id and status='open' ----
            ticket_data['channel_id'] = channel.id
            ticket_data['status'] = 'open'
            try:
                await self.data_manager.async_save_ticket(ticket_data)
            except Exception as e:
                # DB update failed after the channel exists. Try to clean up the
                # channel so we don't leave an orphan channel with no DB record.
                logging.error(f"[TicketTool] DB save failed after channel creation: {e}")
                try:
                    await channel.delete(reason="Ticket DB record could not be saved")
                except Exception:
                    pass
                return None, "Ticket channel was created but could not be saved. Please try again."
        
        # ---- STEP 4: persist panel answers (non-fatal if this fails) ----
        # Outside the lock: answer persistence doesn't affect the limit check.
        if answers:
            for qid, answer_text in answers.items():
                try:
                    await self.data_manager.async_save_ticket_answer({
                        'answer_id': str(uuid.uuid4())[:8],
                        'ticket_id': ticket_id,
                        'question_id': qid,
                        'user_id': user.id,
                        'answer_text': answer_text,
                        'answered_at': datetime.now(timezone.utc).isoformat()
                    })
                except Exception as e:
                    logging.warning(f"[TicketTool] Failed to save answer for {ticket_id}: {e}")
        
        return channel, ticket_id
    
    async def close_ticket(
        self,
        channel: discord.TextChannel,
        closed_by: discord.Member,
        reason: str = "No reason provided"
    ) -> bool:
        """Close a ticket and generate transcript.

        The open→closing transition is done atomically via
        DataManager.atomic_begin_closing so two near-simultaneous close
        requests (e.g. a staff button click racing the ticket creator's close
        command) can't both proceed to generate a transcript and delete the
        channel. Only the first caller wins the UPDATE; the second sees
        rowcount==0 and returns False immediately.
        """
        ticket = await self.data_manager.async_load_ticket_by_channel(channel.id)
        if not ticket:
            return False

        if ticket.get('status') in ('closed', 'closing'):
            return False

        # Atomic open→closing transition. If this returns False, another caller
        # already started closing (or the ticket was closed/not-found between
        # our load and this UPDATE). Bail out — do NOT generate a transcript.
        won = await asyncio.to_thread(
            self.data_manager.atomic_begin_closing,
            ticket['ticket_id'], closed_by.id, reason,
        )
        if not won:
            logging.info(f"[TicketTool] close_ticket lost the race for {ticket['ticket_id']} (already closing/closed); aborting to avoid duplicate transcript.")
            return False

        # Reload so the in-memory dict reflects the committed 'closing' state
        # (status / closed_by / close_reason) before transcript generation.
        ticket = await self.data_manager.async_load_ticket(ticket['ticket_id'])
        if not ticket:
            logging.error("[TicketTool] Ticket disappeared after atomic_begin_closing; cannot generate transcript.")
            return False
        try:
            transcript = await self._generate_transcript(channel, ticket, closed_by)
        except Exception as e:
            logging.error(f"[TicketTool] Transcript generation failed, aborting close: {e}")
            # Revert to 'open' so the ticket can be retried. Use a conditional
            # UPDATE so we don't clobber a concurrent state change.
            try:
                await asyncio.to_thread(
                    self.data_manager.atomic_revert_closing, ticket['ticket_id']
                )
            except Exception:
                pass
            return False
        
        ticket['status'] = 'closed'
        ticket['closed_at'] = datetime.now(timezone.utc).isoformat()
        try:
            await self.data_manager.async_save_ticket(ticket)
        except Exception as e:
            logging.error(f"[TicketTool] Could not persist final 'closed' status: {e}")
        
        settings = await self.data_manager.async_load_ticket_settings(channel.guild.id) \
            if hasattr(self.data_manager, 'async_load_ticket_settings') \
            else self.data_manager.load_ticket_settings(channel.guild.id)
        
        if settings and settings.get('transcripts_channel_id'):
            transcripts_channel = channel.guild.get_channel(settings['transcripts_channel_id'])
            if transcripts_channel:
                try:
                    await transcripts_channel.send(embed=transcript['embed'], file=transcript['file'])
                except Exception as e:
                    logging.warning(f"[Tickets] Could not post transcript to transcripts channel: {e}")
        
        if settings and settings.get('dm_transcripts') and ows_get("dm_transcript_on_close"):
            try:
                creator = channel.guild.get_member(ticket['creator_id'])
                if creator:
                    from io import BytesIO
                    dm_file = discord.File(
                        BytesIO(transcript['html'].encode('utf-8')),
                        filename=f"transcript-{ticket['ticket_id']}.html"
                    )
                    await creator.send(
                        embed=discord.Embed(
                            title=f"Ticket Closed - {channel.guild.name}",
                            description=f"Your ticket has been closed.\n**Reason:** {reason}\n\nYour transcript is attached below.",
                            color=discord.Color.orange()
                        ),
                        file=dm_file
                    )
            except Exception as e:
                logging.warning(f"[Tickets] Could not DM transcript to user: {e}")
        
        try:
            await channel.delete(reason=f"Ticket closed by {closed_by}: {reason}")
        except Exception as e:
            logging.warning(f"[Tickets] Could not delete channel {channel.id}: {e}")
        return True
    
    def _is_ticket_staff(self, member: discord.Member, ticket: Dict) -> bool:
        """Return True if `member` is allowed to act as staff on this ticket.

        Staff = a member with Manage Channels/Administrator, OR a member who
        holds the ticket's support role (resolved from the ticket's panel
        first, falling back to the guild ticket settings). The ticket creator
        is NEVER considered staff for their own ticket — this is what prevents
        a user from claiming their own ticket via the Claim button.
        """
        if member is None:
            return False
        try:
            if member.guild_permissions.administrator or member.guild_permissions.manage_channels:
                return True
        except Exception:
            pass

        support_role_id = None
        panel_id = ticket.get('panel_id') if ticket else None
        if panel_id:
            try:
                panel = self.data_manager.load_ticket_panel(panel_id)
                if panel:
                    support_role_id = panel.get('support_role_id')
            except Exception:
                pass
        if not support_role_id:
            try:
                settings = self.data_manager.load_ticket_settings(member.guild.id)
                if settings:
                    support_role_id = settings.get('support_role_id')
            except Exception:
                pass
        if support_role_id:
            role = member.guild.get_role(support_role_id)
            if role and role in member.roles:
                return True
        return False

    async def claim_ticket(self, channel: discord.TextChannel, user: discord.Member) -> Tuple[bool, str]:
        """Claim a ticket atomically.

        Uses a single conditional UPDATE so two staff members clicking Claim
        at nearly the same time can't both succeed — only the first UPDATE
        affects a row, the second is a no-op.

        PERMISSION FIX: only staff (Manage Channels / Administrator / support
        role) may claim, and the ticket creator is explicitly blocked from
        claiming their own ticket. Without this, a ticket creator who has
        send_messages access to the channel could click Claim and take
        ownership of their own support ticket.
        """
        ticket = await self.data_manager.async_load_ticket_by_channel(channel.id)
        if not ticket:
            return False, "This is not a ticket channel."

        # The ticket creator may never claim their own ticket.
        if ticket.get('creator_id') == user.id:
            return False, "You can't claim your own ticket. Please wait for staff to respond."

        # Only staff may claim.
        if not self._is_ticket_staff(user, ticket):
            return False, "Only staff can claim tickets."

        success, msg = await asyncio.to_thread(
            self.data_manager.atomic_claim_ticket, ticket['ticket_id'], user.id
        )
        if not success:
            # Already claimed — return a friendly message naming the claimer.
            existing_id = ticket.get('claimed_by')
            if not existing_id:
                # Reload to get the freshly written claimer.
                fresh = await self.data_manager.async_load_ticket(ticket['ticket_id'])
                existing_id = fresh.get('claimed_by') if fresh else None
            if existing_id:
                claimer = channel.guild.get_member(existing_id)
                return False, f"This ticket is already claimed by {claimer.mention if claimer else 'someone'}."
            return False, msg
        return True, f"Ticket claimed by {user.mention}"
    
    async def unclaim_ticket(self, channel: discord.TextChannel, user: discord.Member) -> Tuple[bool, str]:
        """Release a ticket claim."""
        ticket = self.data_manager.load_ticket_by_channel(channel.id)
        if not ticket:
            return False, "This is not a ticket channel."
        
        if not ticket.get('claimed_by'):
            return False, "This ticket is not claimed."
        
        if ticket['claimed_by'] != user.id and not user.guild_permissions.administrator:
            return False, "You can only unclaim tickets you claimed (or be admin)."
        
        ticket['claimed_by'] = None
        ticket['claimed_at'] = None
        self.data_manager.save_ticket(ticket)
        
        return True, "Ticket unclaimed."
    
    async def _generate_transcript(self, channel: discord.TextChannel, ticket: Dict, closed_by: discord.Member) -> Dict:
        """Generate HTML transcript like Ticket Tool."""
        import uuid
        from io import BytesIO
        
        messages = []
        message_count = 0
        
        # limit=None pulls the entire channel history. Discord paginates this
        # under the hood (100 msgs per request), so very long tickets take
        # longer but are no longer silently truncated at 500 messages.
        async for msg in channel.history(limit=None, oldest_first=True):
            if msg.author.bot and msg.embeds:
                continue  # Skip bot embeds
            
            message_count += 1
            messages.append({
                'author_id': msg.author.id,
                'author_name': msg.author.display_name,
                'author_avatar': str(msg.author.avatar.url) if msg.author.avatar else str(msg.author.default_avatar.url),
                'content': msg.content,
                'attachments': [att.url for att in msg.attachments],
                'timestamp': msg.created_at.strftime('%Y-%m-%d %H:%M:%S'),
                'embeds': len(msg.embeds)
            })

        # FALLBACK: if channel history is empty (e.g. messages were
        # bulk-deleted, or the channel was partially lost before close),
        # reconstruct the message list from the ticket_messages backup table
        # that on_message populates. This keeps the transcript usable even
        # when Discord's own history is gone, and gives the previously-unused
        # ticket_messages table a real purpose.
        if not messages:
            try:
                backup = await self.data_manager.async_load_ticket_messages(ticket['ticket_id'])
            except Exception as exc:
                logging.warning(f"[TicketTool] ticket_messages fallback load failed: {exc}")
                backup = []
            for row in backup:
                raw_atts = row.get('attachments')
                if isinstance(raw_atts, str):
                    try:
                        atts = json.loads(raw_atts) if raw_atts else []
                    except Exception:
                        atts = []
                elif isinstance(raw_atts, list):
                    atts = raw_atts
                else:
                    atts = []
                # Normalize the stored created_at ISO timestamp into the
                # 'YYYY-MM-DD HH:MM:SS' display format the transcript expects.
                ts_raw = row.get('created_at') or ''
                try:
                    parsed = datetime.fromisoformat(ts_raw.replace('Z', '+00:00'))
                    ts_disp = parsed.strftime('%Y-%m-%d %H:%M:%S')
                except Exception:
                    ts_disp = ts_raw[:19] if ts_raw else ''
                messages.append({
                    'author_id': row.get('author_id'),
                    'author_name': row.get('author_name', 'Unknown'),
                    'author_avatar': row.get('author_avatar', ''),
                    'content': row.get('content', ''),
                    'attachments': atts,
                    'timestamp': ts_disp,
                    'embeds': 0,
                })
            message_count = len(messages)
            if messages:
                logging.info(f"[TicketTool] Transcript used ticket_messages fallback ({message_count} rows) for {ticket['ticket_id']}")

        # Generate HTML (all user-controlled values are escaped inside).
        html_content = self._generate_html_transcript(channel, ticket, messages, closed_by)
        
        # Save transcript to database
        transcript_id = str(uuid.uuid4())[:8]
        transcript_data = {
            'transcript_id': transcript_id,
            'ticket_id': ticket['ticket_id'],
            'guild_id': channel.guild.id,
            'channel_id': channel.id,
            'creator_id': ticket['creator_id'],
            'closed_by': closed_by.id,
            'claimed_by': ticket.get('claimed_by'),
            'category': ticket.get('category'),
            'created_at': ticket.get('created_at'),
            'closed_at': datetime.now(timezone.utc).isoformat(),
            'message_count': message_count,
            'html_content': html_content
        }
        await self.data_manager.async_save_transcript(transcript_data)
        
        # Create embed with better formatting like Ticket Tool
        embed = discord.Embed(
            title=f"📋 Ticket Transcript - {ticket['ticket_id']}",
            description=f"**Category:** {ticket.get('category', 'General')}\n**Subject:** {ticket.get('subject', 'N/A')}",
            color=discord.Color.blue(),
            timestamp=datetime.now(timezone.utc)
        )
        
        creator = channel.guild.get_member(ticket['creator_id'])
        embed.add_field(name="👤 Creator", value=creator.mention if creator else f"<@{ticket['creator_id']}>", inline=True)
        embed.add_field(name="🔒 Closed By", value=closed_by.mention, inline=True)
        embed.add_field(name="💬 Messages", value=str(message_count), inline=True)
        
        if ticket.get('claimed_by'):
            claimer = channel.guild.get_member(ticket['claimed_by'])
            embed.add_field(name="🙋 Claimed By", value=claimer.mention if claimer else f"<@{ticket['claimed_by']}>", inline=True)
        
        # Add message preview (first 5 messages) - this was missing!
        if messages:
            preview_text = ""
            for i, msg in enumerate(messages[:5]):
                content_preview = msg.get('content', '')[:100]
                if len(msg.get('content', '')) > 100:
                    content_preview += "..."
                preview_text += f"**{msg.get('author_name', 'Unknown')}:** {content_preview}\n"
            if len(messages) > 5:
                preview_text += f"\n*...and {len(messages) - 5} more messages*"
            embed.add_field(name="📝 Message Preview", value=preview_text or "No messages", inline=False)
        
        # Add ticket duration
        if ticket.get('created_at'):
            try:
                created = datetime.fromisoformat(ticket['created_at'].replace('Z', '+00:00'))
                duration = datetime.now(timezone.utc) - created
                hours, remainder = divmod(int(duration.total_seconds()), 3600)
                minutes, seconds = divmod(remainder, 60)
                duration_str = f"{hours}h {minutes}m {seconds}s" if hours > 0 else f"{minutes}m {seconds}s"
                embed.add_field(name="⏱️ Duration", value=duration_str, inline=True)
            except:
                pass
        
        embed.set_footer(text=f"Ticket ID: {ticket['ticket_id']} • Download HTML for full transcript")
        
        # Create file
        file = discord.File(
            BytesIO(html_content.encode('utf-8')),
            filename=f"transcript-{ticket['ticket_id']}.html"
        )
        
        return {'embed': embed, 'file': file, 'html': html_content}
    
    def _generate_html_transcript(self, channel: discord.TextChannel, ticket: Dict, messages: List[Dict], closed_by: discord.Member) -> str:
        """Generate a Ticket Tool style HTML transcript.

        SECURITY: every value that comes from a user-controlled source
        (message content, display names, attachment URLs, panel names) is
        HTML-escaped via _esc() / _safe_url() before being interpolated.
        This closes the XSS vector where a ticket message containing
        `<img src=x onerror=...>` would execute when the transcript was
        opened in a browser.
        """
        guild = channel.guild
        creator = guild.get_member(ticket['creator_id'])

        # Pre-escape header values.
        esc_ticket_id = _esc(ticket['ticket_id'])
        esc_category = _esc(ticket.get('category', 'General'))
        esc_created_at = _esc((ticket.get('created_at') or 'N/A')[:19])
        esc_creator = _esc(creator.display_name if creator else 'Unknown')
        esc_closed_by = _esc(closed_by.display_name)
        esc_bot_name = _esc(self.bot.user.name if self.bot.user else 'Bot')
        claimed_by = ticket.get('claimed_by')
        closed_now = _esc(datetime.now().strftime('%Y-%m-%d %H:%M:%S'))

        html = f"""<!DOCTYPE html>
<html>
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Ticket Transcript - {esc_ticket_id}</title>
    <style>
        * {{ margin: 0; padding: 0; box-sizing: border-box; }}
        body {{ 
            font-family: 'Segoe UI', Tahoma, Geneva, Verdana, sans-serif;
            background: linear-gradient(135deg, #1a1a2e 0%, #16213e 100%);
            min-height: 100vh;
            color: #fff;
        }}
        .container {{ max-width: 900px; margin: 0 auto; padding: 20px; }}
        .header {{
            background: linear-gradient(135deg, #5865F2 0%, #7289DA 100%);
            padding: 30px;
            border-radius: 15px;
            margin-bottom: 20px;
            box-shadow: 0 10px 30px rgba(0,0,0,0.3);
        }}
        .header h1 {{ font-size: 28px; margin-bottom: 10px; }}
        .header .info {{ display: flex; gap: 20px; flex-wrap: wrap; margin-top: 15px; }}
        .header .info-item {{ 
            background: rgba(255,255,255,0.1);
            padding: 8px 15px;
            border-radius: 8px;
            font-size: 14px;
        }}
        .messages {{ background: #2f3136; border-radius: 15px; overflow: hidden; }}
        .message {{
            padding: 15px 20px;
            border-bottom: 1px solid #36393f;
            display: flex;
            gap: 15px;
        }}
        .message:hover {{ background: rgba(79, 84, 92, 0.16); }}
        .message:last-child {{ border-bottom: none; }}
        .message-avatar {{ width: 40px; height: 40px; border-radius: 50%; flex-shrink: 0; }}
        .message-content {{ flex: 1; }}
        .message-header {{ display: flex; align-items: center; gap: 10px; margin-bottom: 5px; }}
        .message-author {{ font-weight: 600; color: #fff; }}
        .message-timestamp {{ font-size: 12px; color: #72767d; }}
        .message-text {{ color: #dcddde; line-height: 1.5; word-wrap: break-word; white-space: pre-wrap; }}
        .attachment {{
            background: #2f3136;
            border: 1px solid #4f545c;
            border-radius: 8px;
            padding: 10px;
            margin-top: 8px;
            display: inline-block;
        }}
        .attachment a {{ color: #00b0f4; text-decoration: none; }}
        .footer {{
            text-align: center;
            padding: 20px;
            color: #72767d;
            font-size: 14px;
        }}
        .claimed-badge {{
            background: #faa61a;
            color: #000;
            padding: 3px 8px;
            border-radius: 4px;
            font-size: 12px;
            margin-left: 10px;
        }}
    </style>
</head>
<body>
    <div class="container">
        <div class="header">
            <h1>Ticket Transcript</h1>
            <div class="info">
                <div class="info-item">ID: {esc_ticket_id}</div>
                <div class="info-item">Category: {esc_category}</div>
                <div class="info-item">Created: {esc_created_at}</div>
                <div class="info-item">Closed: {closed_now}</div>
            </div>
            <div class="info" style="margin-top: 10px;">
                <div class="info-item">Creator: {esc_creator}</div>
                <div class="info-item">Closed By: {esc_closed_by}</div>
                {f'<div class="info-item">Claimed By: {_esc(claimed_by)}</div>' if claimed_by else ''}
            </div>
        </div>
        <div class="messages">
"""
        for msg in messages:
            # Escape every user-controlled field. content is rendered with
            # white-space: pre-wrap so newlines are preserved without <br>.
            esc_author_name = _esc(msg.get('author_name', 'Unknown'))
            esc_author_avatar = _safe_url(msg.get('author_avatar', ''))
            esc_timestamp = _esc(msg.get('timestamp', ''))
            content = msg.get('content') or ''
            esc_content = _esc(content) if content else '<em>No content</em>'

            html += f"""
            <div class="message">
                <img class="message-avatar" src="{esc_author_avatar}" alt="Avatar">
                <div class="message-content">
                    <div class="message-header">
                        <span class="message-author">{esc_author_name}</span>
                        <span class="message-timestamp">{esc_timestamp}</span>
                    </div>
                    <div class="message-text">{esc_content}</div>
"""
            for att in (msg.get('attachments') or []):
                safe_att = _safe_url(att)
                if safe_att:
                    html += f"""
                    <div class="attachment"><a href="{safe_att}" target="_blank" rel="noopener noreferrer">📎 Attachment</a></div>
"""
            html += """
                </div>
            </div>
"""

        html += f"""
        </div>
        <div class="footer">
            Generated by {esc_bot_name} • {closed_now}
        </div>
    </div>
</body>
</html>"""

        return html


# Global ticket tool instance
ticket_tool: Optional[TicketToolSystem] = None


# --- TICKET PANEL VIEW (The panel message with create button) ---
PANEL_BUTTON_PREFIX = "create_ticket:"


class TicketPanelView(View):
    """The panel that users interact with to create tickets.

    PERSISTENCE FIX: each panel's button uses a UNIQUE custom_id of the form
    `create_ticket:{panel_id}`. Previously every panel used the same
    `create_ticket_button` custom_id, so discord.py could only keep ONE
    registered panel config after restart — all panel messages would then
    route to that single panel's config. With per-panel custom_ids, each
    panel message correctly resolves to its own panel even after restart.
    """

    def __init__(self, panel: Dict):
        super().__init__(timeout=None)
        self.panel = panel
        panel_id = panel.get('panel_id', 'unknown')

        # Configure button based on panel settings, AND give it a unique
        # custom_id so persistence routing is unambiguous.
        self.create_button.style = discord.ButtonStyle(panel.get('button_style', 3))
        self.create_button.label = panel.get('button_label', 'Create Ticket')
        if panel.get('button_emoji'):
            self.create_button.emoji = panel['button_emoji']
        self.create_button.custom_id = f"{PANEL_BUTTON_PREFIX}{panel_id}"

    @staticmethod
    def _panel_id_from_custom_id(custom_id: str) -> Optional[str]:
        if custom_id and custom_id.startswith(PANEL_BUTTON_PREFIX):
            return custom_id[len(PANEL_BUTTON_PREFIX):]
        return None

    @discord.ui.button(label="Create Ticket", style=discord.ButtonStyle.success, custom_id="create_ticket:placeholder")
    async def create_button(self, interaction: discord.Interaction, button: Button) -> None:
        if not ticket_tool:
            await interaction.response.send_message("Ticket system not initialized.", ephemeral=True)
            return

        # Owner Settings gate: refuse new ticket creation when the Tickets
        # System is disabled. Checked at the panel button (the user-facing
        # entry point) AND inside create_ticket() so both paths are covered.
        if not ows_get("enable_tickets"):
            await interaction.response.send_message(
                "The ticket system is currently disabled by the server owner. Please try again later.",
                ephemeral=True,
            )
            return

        panel_id = self._panel_id_from_custom_id(button.custom_id)
        panel = None
        if panel_id:
            panel = ticket_tool.data_manager.load_ticket_panel(panel_id)
        if not panel:
            panel = self.panel
        if not panel:
            await interaction.response.send_message("This ticket panel no longer exists.", ephemeral=True)
            return

        if ows_get("ticket_blacklist"):
            blacklisted, reason = ticket_tool.data_manager.is_user_blacklisted(interaction.guild.id, interaction.user.id)
            if blacklisted:
                await interaction.response.send_message(f"You are blacklisted from creating tickets. Reason: {reason}", ephemeral=True)
                return

        user_tickets = ticket_tool.data_manager.load_tickets_by_creator(interaction.user.id, interaction.guild.id)
        ticket_limit = panel.get('ticket_limit', 3)
        if len(user_tickets) >= ticket_limit:
            await interaction.response.send_message(
                f"You already have {len(user_tickets)} open ticket(s). Close one before creating another.",
                ephemeral=True
            )
            return

        questions = ticket_tool.data_manager.load_panel_questions(panel['panel_id'])

        if questions:
            await interaction.response.send_modal(TicketQuestionsModal(panel, questions))
        else:
            await interaction.response.defer(thinking=True, ephemeral=True)
            channel, ticket_id = await ticket_tool.create_ticket(
                interaction.guild, interaction.user, panel
            )
            if channel:
                await self._send_welcome_message(channel, interaction.user, panel)
                await interaction.followup.send(f"Ticket created: {channel.mention}", ephemeral=True)
            else:
                await interaction.followup.send(f"Failed to create ticket: {ticket_id}", ephemeral=True)

    async def _send_welcome_message(self, channel: discord.TextChannel, user: discord.Member, panel: Dict) -> None:
        """Send the welcome message in the ticket channel."""
        ticket = await ticket_tool.data_manager.async_load_ticket_by_channel(channel.id)
        if not ticket:
            return

        view = TicketControlView(ticket['ticket_id'])

        welcome_text = panel.get('welcome_message', "Support will be with you shortly.")

        embed = discord.Embed(
            title=f"Ticket #{ticket['ticket_id']}",
            description=f"Welcome {user.mention}!\n\n{welcome_text}\n\n**Category:** {panel.get('name', 'General')}",
            color=discord.Color(panel.get('embed_color', 0x5865F2)),
            timestamp=datetime.now(timezone.utc)
        )

        if panel.get('embed_thumbnail'):
            embed.set_thumbnail(url=panel['embed_thumbnail'])
        if panel.get('embed_image'):
            embed.set_image(url=panel['embed_image'])

        embed.set_footer(text=f"Created by {user.display_name}")

        mention_text = ""
        if ows_get("mention_support_on_create"):
            support_role_id = panel.get('support_role_id')
            if support_role_id:
                role = channel.guild.get_role(support_role_id)
                if role:
                    mention_text = f"{role.mention} "

        await channel.send(f"{mention_text}{user.mention}", embed=embed, view=view)


class TicketQuestionsModal(Modal, title="Create Ticket"):
    """Modal for ticket questions."""
    
    def __init__(self, panel: Dict, questions: List[Dict]):
        super().__init__()
        self.panel = panel
        self.questions = questions
        self.answers = {}
        
        for i, q in enumerate(questions[:5]):  # Discord limits to 5 items
            text_input = TextInput(
                label=q['question_text'][:45],
                placeholder=q.get('placeholder', ''),
                style=discord.TextStyle.paragraph if q.get('question_type') == 'paragraph' else discord.TextStyle.short,
                required=q.get('required', True),
                custom_id=f"question_{q['question_id']}"
            )
            self.add_item(text_input)
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        await interaction.response.defer(thinking=True, ephemeral=True)
        
        # Collect answers
        answers = {}
        for child in self.children:
            qid = child.custom_id.replace('question_', '')
            answers[qid] = child.value
        
        # Create ticket
        channel, ticket_id = await ticket_tool.create_ticket(
            interaction.guild, interaction.user, self.panel, answers=answers
        )
        
        if channel:
            # Send welcome message with answers
            await self._send_welcome_with_answers(channel, interaction.user, answers)
            await interaction.followup.send(f"Ticket created: {channel.mention}", ephemeral=True)
        else:
            await interaction.followup.send(f"Failed to create ticket: {ticket_id}", ephemeral=True)
    
    async def _send_welcome_with_answers(self, channel: discord.TextChannel, user: discord.Member, answers: Dict) -> None:
        """Send welcome message with question answers."""
        ticket = ticket_tool.data_manager.load_ticket_by_channel(channel.id)
        if not ticket:
            return
        
        view = TicketControlView(ticket['ticket_id'])
        
        welcome_text = self.panel.get('welcome_message', "Support will be with you shortly.")
        
        embed = discord.Embed(
            title=f"Ticket #{ticket['ticket_id']}",
            color=discord.Color(self.panel.get('embed_color', 0x5865F2)),
            timestamp=datetime.now(timezone.utc)
        )
        
        embed.add_field(name="Creator", value=user.mention, inline=True)
        embed.add_field(name="Category", value=self.panel.get('name', 'General'), inline=True)
        
        # Add answers - keys are bare question IDs (prefix stripped in on_submit)
        for q in self.questions:
            answer = answers.get(q['question_id'], 'No answer')
            embed.add_field(name=q['question_text'][:256], value=answer[:1024], inline=False)
        
        embed.add_field(name="Info", value=welcome_text, inline=False)
        embed.set_footer(text=f"Created by {user.display_name}")
        
        support_role_id = self.panel.get('support_role_id')
        mention_text = ""
        if support_role_id:
            role = channel.guild.get_role(support_role_id)
            if role:
                mention_text = f"{role.mention} "
        
        await channel.send(f"{mention_text}{user.mention}", embed=embed, view=view)


# --- TICKET CONTROL VIEW (Inside ticket channels) ---
class TicketControlView(View):
    """Buttons inside a ticket channel for control.

    PERSISTENCE FIX: on startup a single `TicketControlView("")` is
    registered (see on_ready). After restart, `self.ticket_id` is the empty
    string, so any handler that used `self.ticket_id` directly (close,
    transcript, note, priority) would fail. Every handler now resolves the
    ticket from `interaction.channel.id` via the DB, which is always correct
    regardless of what the view instance was constructed with.
    """

    def __init__(self, ticket_id: str = ""):
        super().__init__(timeout=None)
        self.ticket_id = ticket_id

    async def _resolve_ticket(self, interaction: discord.Interaction) -> Optional[Dict]:
        """Look up the ticket for the current channel (robust after restart)."""
        if not ticket_tool:
            return None
        ticket_id = self.ticket_id
        ticket = None
        if ticket_id:
            ticket = await ticket_tool.data_manager.async_load_ticket(ticket_id)
        if not ticket and interaction.channel:
            # Fallback: resolve by channel — always correct inside a ticket channel.
            ticket = await ticket_tool.data_manager.async_load_ticket_by_channel(interaction.channel.id)
        return ticket

    @discord.ui.button(label="Claim", style=discord.ButtonStyle.success, emoji="🙋", custom_id="ticket_claim")
    async def claim_button(self, interaction: discord.Interaction, button: Button) -> None:
        if not ticket_tool:
            await interaction.response.send_message("System not ready.", ephemeral=True)
            return

        success, message = await ticket_tool.claim_ticket(interaction.channel, interaction.user)

        if success:
            button.disabled = True
            button.label = "Claimed"
            button.style = discord.ButtonStyle.secondary
            await interaction.response.edit_message(view=self)
            await interaction.channel.send(f"Ticket claimed by {interaction.user.mention}")
        else:
            await interaction.response.send_message(message, ephemeral=True)

    @discord.ui.button(label="Close", style=discord.ButtonStyle.danger, emoji="🔒", custom_id="ticket_close")
    async def close_button(self, interaction: discord.Interaction, button: Button) -> None:
        # Resolve the real ticket_id from the channel so closing works even
        # when this view instance was the empty-string startup registration.
        ticket = await self._resolve_ticket(interaction)
        if not ticket:
            await interaction.response.send_message("Ticket not found.", ephemeral=True)
            return
        await interaction.response.send_modal(CloseTicketModal(ticket['ticket_id']))

    @discord.ui.button(label="Transcript", style=discord.ButtonStyle.secondary, emoji="📜", custom_id="ticket_transcript")
    async def transcript_button(self, interaction: discord.Interaction, button: Button) -> None:
        ticket = await self._resolve_ticket(interaction)
        if not ticket:
            await interaction.response.send_message("Ticket not found.", ephemeral=True)
            return

        transcript = await ticket_tool._generate_transcript(
            interaction.channel, ticket, interaction.user
        )
        await interaction.response.send_message(
            embed=transcript['embed'],
            file=transcript['file'],
            ephemeral=True
        )

    @discord.ui.button(label="Note", style=discord.ButtonStyle.secondary, emoji="📝", row=1, custom_id="ticket_note")
    async def note_button(self, interaction: discord.Interaction, button: Button) -> None:
        """Add a staff note. See AddNoteModal — notes are now stored privately
        (ephemeral confirmation only, no public channel post)."""
        ticket = await self._resolve_ticket(interaction)
        if not ticket:
            await interaction.response.send_message("Ticket not found.", ephemeral=True)
            return
        if not interaction.user.guild_permissions.manage_channels:
            await interaction.response.send_message("Only staff can add notes.", ephemeral=True)
            return
        await interaction.response.send_modal(AddNoteModal(ticket['ticket_id']))

    @discord.ui.button(label="Priority", style=discord.ButtonStyle.secondary, emoji="🚨", row=1, custom_id="ticket_priority")
    async def priority_button(self, interaction: discord.Interaction, button: Button) -> None:
        """Set the priority of this ticket."""
        if not interaction.user.guild_permissions.manage_channels:
            await interaction.response.send_message("Only staff can set priority.", ephemeral=True)
            return
        ticket = await self._resolve_ticket(interaction)
        if not ticket:
            await interaction.response.send_message("Ticket not found.", ephemeral=True)
            return
        await interaction.response.send_message(
            "Select a priority level:",
            view=PrioritySelectView(ticket['ticket_id']),
            ephemeral=True
        )


class CloseTicketModal(Modal, title="Close Ticket"):
    reason_input = TextInput(
        label="Close Reason",
        placeholder="Why are you closing this ticket?",
        style=discord.TextStyle.paragraph,
        required=False
    )
    
    def __init__(self, ticket_id: str):
        super().__init__()
        self.ticket_id = ticket_id
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        reason = self.reason_input.value or "No reason provided"
        
        ticket = ticket_tool.data_manager.load_ticket(self.ticket_id)
        is_creator = ticket and ticket.get('creator_id') == interaction.user.id
        
        if is_creator and ows_get("ticket_rating_prompt"):
            view = TicketRatingView(self.ticket_id, reason, interaction.channel, interaction.user)
            await interaction.response.send_message(
                "⭐ **Please rate your support experience before closing:**",
                view=view,
                ephemeral=True
            )
        else:
            await interaction.response.send_message(
                "Are you sure you want to close this ticket?",
                view=ConfirmCloseView(self.ticket_id, reason),
                ephemeral=True
            )


class ConfirmCloseView(View):
    """Confirmation view for staff closing tickets."""
    
    def __init__(self, ticket_id: str, reason: str):
        super().__init__(timeout=60)
        self.ticket_id = ticket_id
        self.reason = reason
    
    @discord.ui.button(label="✅ Close Ticket", style=discord.ButtonStyle.danger)
    async def confirm_close(self, interaction: discord.Interaction, button: Button) -> None:
        await interaction.response.defer(thinking=True)
        
        # Disable buttons
        for child in self.children:
            child.disabled = True
        await interaction.edit_original_response(view=self)
        
        # Now close the ticket
        success = await ticket_tool.close_ticket(interaction.channel, interaction.user, self.reason)
        if not success:
            await interaction.followup.send("Failed to close ticket.", ephemeral=True)
    
    @discord.ui.button(label="❌ Cancel", style=discord.ButtonStyle.secondary)
    async def cancel_close(self, interaction: discord.Interaction, button: Button) -> None:
        for child in self.children:
            child.disabled = True
        await interaction.response.edit_message(
            content="Close cancelled.",
            view=self
        )


class TicketRatingView(View):
    """Rating view BEFORE ticket close - gives user time to rate."""
    
    def __init__(self, ticket_id: str, reason: str, channel: discord.TextChannel, user: discord.Member):
        super().__init__(timeout=120)  # 2 minutes to rate
        self.ticket_id = ticket_id
        self.reason = reason
        self.channel = channel
        self.user = user
        self.rated = False
    
    async def on_timeout(self) -> None:
        """Close ticket after timeout if not rated."""
        if not self.rated and self.channel:
            try:
                await ticket_tool.close_ticket(self.channel, self.user, self.reason)
            except:
                pass  # Channel might already be deleted
    
    @discord.ui.button(label="⭐", style=discord.ButtonStyle.secondary, row=0)
    async def rate_1(self, interaction: discord.Interaction, button: Button) -> None:
        await self._submit_rating(interaction, 1)
    
    @discord.ui.button(label="⭐⭐", style=discord.ButtonStyle.secondary, row=0)
    async def rate_2(self, interaction: discord.Interaction, button: Button) -> None:
        await self._submit_rating(interaction, 2)
    
    @discord.ui.button(label="⭐⭐⭐", style=discord.ButtonStyle.secondary, row=0)
    async def rate_3(self, interaction: discord.Interaction, button: Button) -> None:
        await self._submit_rating(interaction, 3)
    
    @discord.ui.button(label="⭐⭐⭐⭐", style=discord.ButtonStyle.secondary, row=1)
    async def rate_4(self, interaction: discord.Interaction, button: Button) -> None:
        await self._submit_rating(interaction, 4)
    
    @discord.ui.button(label="⭐⭐⭐⭐⭐", style=discord.ButtonStyle.success, row=1)
    async def rate_5(self, interaction: discord.Interaction, button: Button) -> None:
        await self._submit_rating(interaction, 5)
    
    @discord.ui.button(label="Skip & Close", style=discord.ButtonStyle.danger, row=2)
    async def skip_rating(self, interaction: discord.Interaction, button: Button) -> None:
        self.rated = True
        await interaction.response.defer(thinking=True)
        
        # Disable all buttons
        for child in self.children:
            child.disabled = True
        await interaction.edit_original_response(
            content="Closing ticket without rating...",
            view=self
        )
        
        # Close the ticket
        success = await ticket_tool.close_ticket(self.channel, self.user, self.reason)
        if not success:
            await interaction.followup.send("Failed to close ticket.", ephemeral=True)
    
    async def _submit_rating(self, interaction: discord.Interaction, rating: int) -> None:
        self.rated = True
        
        # Save rating
        ticket = ticket_tool.data_manager.load_ticket(self.ticket_id)
        if ticket:
            ticket['rating'] = rating
            ticket_tool.data_manager.save_ticket(ticket)
        
        # Disable all buttons
        for child in self.children:
            child.disabled = True
        
        await interaction.response.edit_message(
            content=f"⭐ Thank you for rating! You gave **{rating} star(s)**.\n\nClosing ticket in 3 seconds...",
            view=self
        )
        
        # Wait a moment then close
        await asyncio.sleep(3)
        
        # Close the ticket
        try:
            await ticket_tool.close_ticket(self.channel, self.user, self.reason)
        except:
            pass  # Channel might already be deleted


# --- ADD NOTE MODAL ---
class AddNoteModal(Modal, title="Add Staff Note"):
    note_input = TextInput(
        label="Note (only staff can see this)",
        style=discord.TextStyle.paragraph,
        placeholder="Internal note visible only to staff...",
        max_length=1000,
        required=True
    )

    def __init__(self, ticket_id: str):
        super().__init__()
        self.ticket_id = ticket_id

    async def on_submit(self, interaction: discord.Interaction) -> None:
        import uuid
        note = {
            'note_id': str(uuid.uuid4())[:8],
            'ticket_id': self.ticket_id,
            'guild_id': interaction.guild.id,
            'author_id': interaction.user.id,
            'content': self.note_input.value,
            'created_at': datetime.now(timezone.utc).isoformat(),
        }
        await data_manager.async_save_ticket_note(note)

        confirm_embed = discord.Embed(
            title="📝 Staff Note Added",
            description="Your note has been saved and is visible to staff via `/notes`.\n\n"
                       f"```\n{self.note_input.value[:1800]}\n```",
            color=discord.Color.yellow(),
            timestamp=datetime.now(timezone.utc)
        )
        confirm_embed.set_footer(text=f"Added by {interaction.user.display_name} • Note ID: {note['note_id']}")

        # PRIVACY FIX: the previous implementation ALSO posted the note content
        # to the ticket channel via `interaction.channel.send(embed=...)`.
        # Because the ticket creator can read that channel, the "private"
        # staff note was anything but private. We now ONLY send the ephemeral
        # confirmation to the staff member and persist the note to the DB,
        # where it can be reviewed with the `/notes` command (staff-only).
        # The transcript still records the note via the saved DB row when
        # generated for staff review.
        await interaction.response.send_message(embed=confirm_embed, ephemeral=True)

        # Optional: forward to a dedicated staff-notes channel if one is
        # configured in ticket settings (`notes_channel_id`). This keeps the
        # note out of the ticket channel the creator can read, while still
        # giving staff a shared place to see new notes.
        settings = data_manager.load_ticket_settings(interaction.guild.id)
        notes_channel_id = settings.get('notes_channel_id') if settings else None
        if notes_channel_id:
            notes_channel = interaction.guild.get_channel(notes_channel_id)
            if notes_channel:
                try:
                    staff_embed = discord.Embed(
                        title=f"📝 Staff Note • Ticket {self.ticket_id}",
                        description=self.note_input.value,
                        color=discord.Color.yellow(),
                        timestamp=datetime.now(timezone.utc)
                    )
                    staff_embed.set_footer(text=f"By {interaction.user.display_name}")
                    await notes_channel.send(embed=staff_embed)
                except Exception as e:
                    logging.warning(f"[Tickets] Could not post note to staff notes channel: {e}")


# --- PRIORITY SELECT VIEW ---
PRIORITY_COLORS = {
    'low':    discord.Color.green(),
    'normal': discord.Color.blue(),
    'high':   discord.Color.orange(),
    'urgent': discord.Color.red(),
}
PRIORITY_EMOJIS = {'low': '🟢', 'normal': '🔵', 'high': '🟠', 'urgent': '🔴'}


class PrioritySelectView(View):
    def __init__(self, ticket_id: str):
        super().__init__(timeout=60)
        self.ticket_id = ticket_id

    @discord.ui.button(label="🟢 Low", style=discord.ButtonStyle.success)
    async def low(self, interaction: discord.Interaction, button: Button) -> None:
        await self._set_priority(interaction, 'low')

    @discord.ui.button(label="🔵 Normal", style=discord.ButtonStyle.primary)
    async def normal(self, interaction: discord.Interaction, button: Button) -> None:
        await self._set_priority(interaction, 'normal')

    @discord.ui.button(label="🟠 High", style=discord.ButtonStyle.secondary)
    async def high(self, interaction: discord.Interaction, button: Button) -> None:
        await self._set_priority(interaction, 'high')

    @discord.ui.button(label="🔴 Urgent", style=discord.ButtonStyle.danger)
    async def urgent(self, interaction: discord.Interaction, button: Button) -> None:
        await self._set_priority(interaction, 'urgent')

    async def _set_priority(self, interaction: discord.Interaction, priority: str) -> None:
        ticket = data_manager.load_ticket(self.ticket_id)
        if not ticket:
            await interaction.response.send_message("Ticket not found.", ephemeral=True)
            return
        ticket['priority'] = priority
        data_manager.save_ticket(ticket)

        emoji = PRIORITY_EMOJIS.get(priority, '')
        color = PRIORITY_COLORS.get(priority, discord.Color.blue())
        embed = discord.Embed(
            title=f"{emoji} Priority Set: {priority.capitalize()}",
            color=color,
            timestamp=datetime.now(timezone.utc)
        )
        embed.set_footer(text=f"Set by {interaction.user.display_name}")
        await interaction.response.edit_message(content=None, embed=embed, view=None)
        await interaction.channel.send(
            embed=discord.Embed(
                description=f"{emoji} Ticket priority set to **{priority.capitalize()}** by {interaction.user.mention}",
                color=color
            )
        )
        self.stop()


# --- PANEL CREATOR VIEW (For creating/editing panels) ---
class PanelCreatorView(View):
    """Interactive panel creator."""
    
    def __init__(self, guild_id: int, user_id: int):
        super().__init__(timeout=600)
        self.guild_id = guild_id
        self.user_id = user_id
        self.panel_data = {
            'panel_id': str(uuid.uuid4())[:8],
            'guild_id': guild_id,
            'name': 'New Panel',
            'embed_title': 'Support Tickets',
            'embed_description': 'Click the button below to create a ticket.',
            'embed_color': 0x5865F2,
            'button_label': 'Create Ticket',
            'button_style': 3,
            'ticket_limit': 3,
            'auto_close_hours': 24,
            'welcome_message': 'Support will be with you shortly.',
            'created_at': datetime.now(timezone.utc).isoformat()
        }
    
    def _create_preview_embed(self) -> discord.Embed:
        embed = discord.Embed(
            title=self.panel_data['embed_title'],
            description=self.panel_data['embed_description'],
            color=discord.Color(self.panel_data['embed_color'])
        )
        embed.set_footer(text=f"Panel: {self.panel_data['name']}")
        return embed
    
    @discord.ui.button(label="Set Name", style=discord.ButtonStyle.primary, row=0)
    async def set_name(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel creator.", ephemeral=True)
            return
        await interaction.response.send_modal(PanelNameModal(self))
    
    @discord.ui.button(label="Set Embed", style=discord.ButtonStyle.primary, row=0)
    async def set_embed(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel creator.", ephemeral=True)
            return
        await interaction.response.send_modal(PanelEmbedModal(self))
    
    @discord.ui.button(label="Set Button", style=discord.ButtonStyle.secondary, row=0)
    async def set_button(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel creator.", ephemeral=True)
            return
        await interaction.response.send_modal(PanelButtonModal(self))
    
    @discord.ui.button(label="Settings", style=discord.ButtonStyle.secondary, row=1)
    async def settings(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel creator.", ephemeral=True)
            return
        await interaction.response.send_modal(PanelSettingsModal(self))
    
    @discord.ui.button(label="Preview", style=discord.ButtonStyle.success, row=1)
    async def preview(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel creator.", ephemeral=True)
            return
        
        preview_view = View(timeout=30)
        preview_view.add_item(Button(
            label=self.panel_data['button_label'],
            style=discord.ButtonStyle(self.panel_data['button_style']),
            disabled=True
        ))
        
        await interaction.response.send_message(
            embed=self._create_preview_embed(),
            view=preview_view,
            ephemeral=True
        )
    
    @discord.ui.button(label="Create Panel", style=discord.ButtonStyle.success, row=2)
    async def create_panel(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Not your panel creator.", ephemeral=True)
            return
        
        # Save panel
        self.panel_data['channel_id'] = interaction.channel.id
        data_manager.save_ticket_panel(self.panel_data)
        
        # Send the actual panel
        view = TicketPanelView(self.panel_data)
        message = await interaction.channel.send(
            embed=self._create_preview_embed(),
            view=view
        )
        
        # Update panel with message ID
        self.panel_data['message_id'] = message.id
        data_manager.save_ticket_panel(self.panel_data)
        
        # Register view for persistence
        interaction.client.add_view(view)
        
        await interaction.response.send_message(
            f"Panel created successfully! Panel ID: `{self.panel_data['panel_id']}`",
            ephemeral=True
        )
        self.stop()


class PanelNameModal(Modal, title="Panel Name"):
    name_input = TextInput(label="Panel Name", placeholder="e.g., Support Tickets", max_length=50)
    
    def __init__(self, view: PanelCreatorView):
        super().__init__()
        self.view = view
        self.name_input.default = view.panel_data.get('name', '')
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        self.view.panel_data['name'] = self.name_input.value
        await interaction.response.send_message(f"Panel name set to: {self.name_input.value}", ephemeral=True)


class PanelEmbedModal(Modal, title="Embed Settings"):
    title_input = TextInput(label="Embed Title", max_length=100)
    desc_input = TextInput(label="Embed Description", style=discord.TextStyle.paragraph, max_length=1000, required=False)
    color_input = TextInput(label="Color (Hex)", max_length=7, placeholder="#5865F2", required=False)
    
    def __init__(self, view: PanelCreatorView):
        super().__init__()
        self.view = view
        self.title_input.default = view.panel_data.get('embed_title', '')
        self.desc_input.default = view.panel_data.get('embed_description', '')
        self.color_input.default = hex(view.panel_data.get('embed_color', 0x5865F2))[2:]
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        self.view.panel_data['embed_title'] = self.title_input.value
        
        if self.desc_input.value:
            self.view.panel_data['embed_description'] = self.desc_input.value
        
        if self.color_input.value:
            try:
                color_hex = self.color_input.value.strip('#')
                self.view.panel_data['embed_color'] = int(color_hex, 16)
            except ValueError:
                pass
        
        await interaction.response.send_message("Embed settings updated!", ephemeral=True)


class PanelButtonModal(Modal, title="Button Settings"):
    label_input = TextInput(label="Button Label", max_length=80, placeholder="Create Ticket")
    emoji_input = TextInput(label="Button Emoji", max_length=50, required=False, placeholder="🎫")
    style_input = TextInput(label="Style (1-4)", max_length=1, placeholder="3")
    
    def __init__(self, view: PanelCreatorView):
        super().__init__()
        self.view = view
        self.label_input.default = view.panel_data.get('button_label', '')
        self.emoji_input.default = view.panel_data.get('button_emoji', '')
        self.style_input.default = str(view.panel_data.get('button_style', 3))
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        self.view.panel_data['button_label'] = self.label_input.value
        
        if self.emoji_input.value:
            self.view.panel_data['button_emoji'] = self.emoji_input.value
        
        try:
            style = int(self.style_input.value)
            if 1 <= style <= 4:
                self.view.panel_data['button_style'] = style
        except ValueError:
            pass
        
        await interaction.response.send_message("Button settings updated!", ephemeral=True)


class PanelSettingsModal(Modal, title="Panel Settings"):
    limit_input = TextInput(label="Max Tickets Per User", max_length=2, placeholder="3")
    close_input = TextInput(label="Auto-Close Hours", max_length=3, placeholder="24")
    welcome_input = TextInput(label="Welcome Message", style=discord.TextStyle.paragraph, max_length=500, required=False)
    
    def __init__(self, view: PanelCreatorView):
        super().__init__()
        self.view = view
        self.limit_input.default = str(view.panel_data.get('ticket_limit', 3))
        self.close_input.default = str(view.panel_data.get('auto_close_hours', 24))
        self.welcome_input.default = view.panel_data.get('welcome_message', '')
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        try:
            self.view.panel_data['ticket_limit'] = int(self.limit_input.value)
        except ValueError:
            pass
        
        try:
            self.view.panel_data['auto_close_hours'] = int(self.close_input.value)
        except ValueError:
            pass
        
        if self.welcome_input.value:
            self.view.panel_data['welcome_message'] = self.welcome_input.value
        
        await interaction.response.send_message("Settings updated!", ephemeral=True)


import uuid





# --- GIVEAWAY SYSTEM (V2 Enhancement) ---
# Per-giveaway locks serialize entry handling so two users clicking at the
# same instant can't both be appended before the entry count / button label
# updates (the race condition that previously left the count briefly wrong).
_giveaway_entry_locks: Dict[str, asyncio.Lock] = {}
_giveaway_locks_guard = threading.Lock()


def _get_giveaway_lock(giveaway_id: str) -> asyncio.Lock:
    """Return (creating if needed) the asyncio.Lock for a given giveaway.

    Locks are created lazily so we don't construct asyncio primitives at import
    time (which can warn on some Python versions if no loop is running yet).
    """
    with _giveaway_locks_guard:
        lock = _giveaway_entry_locks.get(giveaway_id)
        if lock is None:
            lock = asyncio.Lock()
            _giveaway_entry_locks[giveaway_id] = lock
        return lock


def _drop_giveaway_lock(giveaway_id: str) -> None:
    """Free the lock for a giveaway once it has ended (keeps the dict small)."""
    with _giveaway_locks_guard:
        _giveaway_entry_locks.pop(giveaway_id, None)


class GiveawayView(View):
    def __init__(self, giveaway_id: str):
        super().__init__(timeout=None)
        self.giveaway_id = giveaway_id

    @discord.ui.button(label="🎉 Enter Giveaway", style=discord.ButtonStyle.success)
    async def enter_button(self, interaction: discord.Interaction, button: Button) -> None:
        giveaway_id = self.giveaway_id
        if giveaway_id not in giveaways_data:
            await interaction.response.send_message("This giveaway no longer exists.", ephemeral=True)
            return

        # Serialize entries for this giveaway. The check-then-append + label
        # update now happens atomically under the lock, eliminating the race
        # where two near-simultaneous clicks both passed the "already entered"
        # check and both got appended.
        lock = _get_giveaway_lock(giveaway_id)
        async with lock:
            if giveaway_id not in giveaways_data:
                await interaction.response.send_message("This giveaway no longer exists.", ephemeral=True)
                return

            giveaway = giveaways_data[giveaway_id]

            if giveaway.get('status') != 'active':
                await interaction.response.send_message("This giveaway has already ended.", ephemeral=True)
                return

            entries = giveaway.setdefault('entries', [])
            if interaction.user.id in entries:
                await interaction.response.send_message("You've already entered this giveaway!", ephemeral=True)
                return

            entries.append(interaction.user.id)
            
            # Save ONLY this specific giveaway to SQLite (fast, low disk I/O)
            save_giveaways_data(giveaway_id)

            # Update the button label AND the message view in a single
            # interaction response (atomic from the user's perspective).
            button.label = f"🎉 Entered ({len(entries)})"
            try:
                await interaction.response.edit_message(view=self)
            except discord.InteractionResponded:
                # Fallback: response was already made somehow; edit the message.
                try:
                    await interaction.message.edit(view=self)
                except (discord.HTTPException, discord.Forbidden):
                    pass
            except (discord.HTTPException, discord.Forbidden):
                pass

        # Confirmation is sent as a followup so it doesn't conflict with the
        # edit_message response above.
        try:
            await interaction.followup.send("You've been entered! Good luck! 🍀", ephemeral=True)
        except (discord.HTTPException, discord.Forbidden):
            pass


# --- AUTOCOMPLETE FUNCTIONS ---
async def blacklist_autocomplete(interaction: discord.Interaction, current: str) -> List[app_commands.Choice[str]]:
    matching_keywords = [keyword for keyword in blacklisted_keywords if current.lower() in keyword.lower()][:25]
    return [app_commands.Choice(name=keyword[:100], value=keyword[:100]) for keyword in matching_keywords]


async def invite_code_autocomplete(interaction: discord.Interaction, current: str) -> List[app_commands.Choice[str]]:
    if not invite_manager or not invite_manager.tracked_invites:
        return []
    matching_invites = [code for code in invite_manager.tracked_invites.keys() if current.lower() in code.lower()][:25]
    return [app_commands.Choice(name=f"{code} ({invite_manager.tracked_invites[code].get('name', 'Unknown')})", value=code) for code in matching_invites]


# --- INVITE TRACKING VIEW (persistent, attached to the tracking message) ---
class InviteTrackingView(View):
    """Persistent view attached to the invite-tracking message.

    Carries a single 'View Previous Invites' button that opens a paginated
    history modal-ish view. Persisted via `bot.add_view()` on startup and
    re-attached on every `update_invite_message` call so it survives restarts.
    """

    def __init__(self, guild_id: Optional[int]):
        super().__init__(timeout=None)
        self.guild_id = guild_id

    @discord.ui.button(label="View Previous Invites", style=discord.ButtonStyle.secondary, emoji="📜", custom_id="invite_view_history")
    async def view_history_button(self, interaction: discord.Interaction, button: Button) -> None:
        # Anyone with manage_guild can browse the history. Keeps it staff-only
        # so random members can't scrape old invite codes.
        try:
            can_view = interaction.user.guild_permissions.manage_guild
        except Exception:
            can_view = False
        if not can_view:
            await interaction.response.send_message("You need the Manage Server permission to view invite history.", ephemeral=True)
            return

        groups = data_manager.load_invite_history_grouped(guild_id=self.guild_id, limit=500)
        if not groups:
            await interaction.response.send_message("No previous invite batches have been archived yet.", ephemeral=True)
            return

        view = InviteHistoryView(groups, self.guild_id)
        embed = view.build_embed()
        await interaction.response.send_message(embed=embed, view=view, ephemeral=True)


# --- INVITE HISTORY VIEW (paginated, ephemeral) ---
class InviteHistoryView(View):
    """Paginated view of archived invite batches.

    One page = one archived batch (all invites that were saved at the same
    archived_at timestamp). Previous/Next walk the batch list; Close deletes
    the ephemeral message. Bounded to ~25 batches max for safety.
    """

    MAX_PAGES = 25

    def __init__(self, groups: List[Dict[str, Any]], guild_id: Optional[int]):
        super().__init__(timeout=120)
        # Cap to MAX_PAGES so an attacker can't OOM the bot by spamming
        # regenerateinvites thousands of times.
        self.groups = groups[: self.MAX_PAGES]
        self.guild_id = guild_id
        self.page = 0
        self._update_buttons()

    def _update_buttons(self) -> None:
        self.prev_button.disabled = self.page <= 0
        self.next_button.disabled = self.page >= len(self.groups) - 1

    @property
    def current_group(self) -> Dict[str, Any]:
        if not self.groups:
            return {'archived_at': 'unknown', 'archived_by': None, 'invites': []}
        return self.groups[self.page]

    def build_embed(self) -> discord.Embed:
        group = self.current_group
        invites: List[Dict[str, Any]] = group.get('invites', [])

        # Parse the archived_at timestamp into something readable.
        archived_str = group.get('archived_at') or 'unknown'
        try:
            dt = datetime.fromisoformat(archived_str.replace('Z', '+00:00'))
            archived_display = dt.strftime('%Y-%m-%d %H:%M:%S UTC')
        except Exception:
            archived_display = archived_str

        archived_by = group.get('archived_by')
        by_line = f"<@{archived_by}>" if archived_by else "Unknown"

        embed = discord.Embed(
            title=" Invite History — Previous Batch",
            description=(
                f"**Archived:** {archived_display}\n"
                f"**Archived by:** {by_line}\n"
                f"**Invites in batch:** {len(invites)}"
            ),
            color=discord.Color.dark_theme(),
            timestamp=datetime.now(timezone.utc),
        )

        total_uses = 0
        for r in invites:
            code = r.get('invite_code', '?')
            name = r.get('invite_name') or 'Invite'
            max_uses = int(r.get('max_uses', 0) or 0)
            final_uses = int(r.get('final_uses', 0) or 0)
            total_uses += final_uses
            status = r.get('status', 'expired')
            icon = "❌" if status == 'expired' else "✅"
            if max_uses > 0:
                line = f"```diff\n- discord.gg/{code}\n  {name} • {final_uses}/{max_uses} used • {status}\n```"
            else:
                line = f"```diff\n- discord.gg/{code}\n  {name} • {final_uses} used • {status}\n```"
            embed.add_field(name=f"{icon} `{code}` — {name}", value=line, inline=False)

        embed.add_field(name="Batch Summary", value=f"```Total uses: {total_uses}```", inline=False)
        embed.set_footer(text=f"Page {self.page + 1}/{max(1, len(self.groups))} • Use the buttons below to navigate")
        return embed

    @discord.ui.button(label="⬅ Previous", style=discord.ButtonStyle.secondary, custom_id="invite_history_prev")
    async def prev_button(self, interaction: discord.Interaction, button: Button) -> None:
        if self.page > 0:
            self.page -= 1
            self._update_buttons()
            await interaction.response.edit_message(embed=self.build_embed(), view=self)
        else:
            await interaction.response.defer()

    @discord.ui.button(label="Next ➡", style=discord.ButtonStyle.secondary, custom_id="invite_history_next")
    async def next_button(self, interaction: discord.Interaction, button: Button) -> None:
        if self.page < len(self.groups) - 1:
            self.page += 1
            self._update_buttons()
            await interaction.response.edit_message(embed=self.build_embed(), view=self)
        else:
            await interaction.response.defer()

    @discord.ui.button(label="✖ Close", style=discord.ButtonStyle.danger, custom_id="invite_history_close")
    async def close_button(self, interaction: discord.Interaction, button: Button) -> None:
        try:
            await interaction.message.delete()
        except (discord.Forbidden, discord.HTTPException, discord.NotFound):
            try:
                await interaction.response.edit_message(view=None)
            except Exception:
                pass
        self.stop()

    async def on_timeout(self) -> None:
        # Auto-clear the ephemeral message on timeout so it doesn't linger.
        # (Discord auto-hides ephemeral messages after a while anyway, but
        # this keeps the buttons from looking stale if the user reopens.)
        pass


# --- INVITE MANAGER CLASS ---
class InviteManager:
    def __init__(self, bot_instance: commands.Bot):
        self.bot = bot_instance
        self.invite_message_id: Optional[int] = None
        self.invite_channel_id: Optional[int] = None
        self.tracked_invites: Dict[str, Dict[str, Any]] = {}
        self.guild_invites: Dict[str, discord.Invite] = {}
    
    def load_data(self) -> None:
        try:
            self.invite_message_id, self.invite_channel_id, self.tracked_invites = data_manager.load_invites()
            logging.info(f"[InviteManager] Loaded {len(self.tracked_invites)} tracked invites from SQLite")
        except Exception as e:
            logging.error(f"[InviteManager] Error loading data: {e}")

    def save_data(self) -> None:
        try:
            data_manager.save_invites(self.invite_message_id, self.invite_channel_id, self.tracked_invites)
        except Exception as e:
            logging.error(f"[InviteManager] Error saving data: {e}")
    
    async def create_invite(self, guild: discord.Guild, max_uses: int, max_age: int = 0) -> Optional[discord.Invite]:
        for channel in guild.text_channels:
            if channel.permissions_for(guild.me).create_instant_invite:
                invite = await channel.create_invite(max_uses=max_uses, max_age=max_age, unique=True, reason="Auto-generated by GNG Invite Manager")
                logging.info(f"[InviteManager] Created invite {invite.code} with {max_uses} max uses")
                return invite
        return None
    
    async def check_invite_status(self, guild: discord.Guild, invite_code: str) -> Dict[str, Any]:
        try:
            invites = await guild.invites()
            for invite in invites:
                if invite.code == invite_code:
                    return {'valid': True, 'uses': invite.uses, 'max_uses': invite.max_uses, 'expired': invite.uses >= invite.max_uses if invite.max_uses else False}
            return {'valid': False, 'uses': self.tracked_invites.get(invite_code, {}).get('max_uses', 0), 'max_uses': self.tracked_invites.get(invite_code, {}).get('max_uses', 0), 'expired': True}
        except Exception as e:
            logging.error(f"[InviteManager] Error checking invite {invite_code}: {e}")
            return {'valid': False, 'expired': True, 'uses': 0, 'max_uses': 0}
    
    async def generate_all_invites(self, guild: discord.Guild) -> Dict[str, Dict[str, Any]]:
        new_invites: Dict[str, Dict[str, Any]] = {}
        for cfg in INVITE_CONFIGS:
            invite = await self.create_invite(guild, cfg['max_uses'], cfg['max_age'])
            if invite:
                new_invites[invite.code] = {'name': cfg['name'], 'max_uses': cfg['max_uses'], 'uses': 0, 'status': 'active', 'created_at': datetime.now(timezone.utc).isoformat()}
        return new_invites
    
    async def update_invite_message(self, channel: discord.TextChannel) -> bool:
        if not self.invite_message_id:
            return False
        try:
            message = await channel.fetch_message(self.invite_message_id)
        except discord.NotFound:
            self.invite_message_id = None
            return False
        
        try:
            live_invites = await channel.guild.invites()
            live_invite_data = {inv.code: inv for inv in live_invites}
        except Exception:
            live_invite_data = {}
        
        # Build a richer per-invite data list so the embed builder can show
        # progress bars, status icons, and usage counts without re-parsing.
        invite_rows: List[Dict[str, Any]] = []
        all_expired = True

        for code, data in self.tracked_invites.items():
            max_uses = data.get('max_uses', 0)
            current_uses = live_invite_data[code].uses if code in live_invite_data else data.get('uses', 0)
            is_marked_expired = data['status'] == 'expired' or data.get('expired', False)
            remaining = max_uses - current_uses if max_uses > 0 else None

            if is_marked_expired:
                expired = True
            elif max_uses > 0 and remaining <= 0:
                expired = True
                # Sync the tracked record so the next save reflects reality.
                self.tracked_invites[code]['status'] = 'expired'
                self.tracked_invites[code]['expired'] = True
            else:
                expired = False
                all_expired = False

            invite_rows.append({
                'code': code,
                'name': data.get('name', 'Invite'),
                'max_uses': max_uses,
                'uses': current_uses,
                'remaining': remaining,
                'expired': expired,
                'created_at': data.get('created_at'),
            })

        embed = self._build_invite_embed(invite_rows, all_expired)
        embed.set_footer(text=f"Last updated: {datetime.now().strftime('%Y-%m-%d %H:%M:%S')} • {len(invite_rows)} invite(s) tracked")

        # Attach the persistent "View Previous Invites" view so the button on
        # the tracking message keeps working after a bot restart.
        view = InviteTrackingView(channel.guild.id)
        try:
            await message.edit(embed=embed, view=view)
        except discord.HTTPException:
            # If the view edit fails (e.g. discord rejected the components),
            # fall back to just editing the embed so the message still updates.
            await message.edit(embed=embed)
        self.save_data()
        return True
    
    def _build_invite_embed(self, invite_rows: List[Dict[str, Any]], all_expired: bool) -> discord.Embed:
        """Build the main invite-tracking embed.

        Visual improvements over the old version:
          - Per-invite fields (instead of one big code block) so each invite
            gets its own titled slot with usage details.
          - A simple ASCII usage bar gives a quick visual of how used-up an
            invite is (10 segments). Kept intentionally plain so it doesn't
            look over-polished / AI-generated.
          - Status icons (✅ / ⚠️ / ❌) and color shifts make active vs
            expired invites immediately scannable.
          - A compact summary footer line.
        """
        active_rows = [r for r in invite_rows if not r['expired']]
        expired_rows = [r for r in invite_rows if r['expired']]

        if all_expired and self.tracked_invites:
            title = " Invite Links — All Expired"
            color = discord.Color.red()
            description = "All tracked invites have expired. Run `!regenerateinvites` to mint a fresh batch."
        elif not invite_rows:
            title = " Invite Links"
            color = discord.Color.gold()
            description = "No invites are being tracked yet. Run `!setupinvites` to create the first batch."
        else:
            title = " Invite Links"
            color = discord.Color.dark_theme()
            description = f"**{len(active_rows)}** active • **{len(expired_rows)}** expired"

        embed = discord.Embed(title=title, description=description, color=color,
                               timestamp=datetime.now(timezone.utc))

        # Active invites first, then expired — keeps the useful ones on top.
        for r in active_rows:
            embed.add_field(
                name=f"✅ `{r['code']}` — {r['name']}",
                value=self._format_invite_line(r),
                inline=False,
            )
        for r in expired_rows:
            embed.add_field(
                name=f"❌ `{r['code']}` — {r['name']}",
                value=self._format_invite_line(r),
                inline=False,
            )

        # Compact summary footer line.
        total_uses = sum((r['uses'] or 0) for r in invite_rows)
        embed.add_field(
            name="Summary",
            value=f"```Total uses across all links: {total_uses}```",
            inline=False,
        )
        return embed

    @staticmethod
    def _format_invite_line(r: Dict[str, Any]) -> str:
        """Format one invite's stats as a compact, scannable block.

        No progress bars / ASCII bars — just the clean `used/max` fraction
        and a remaining count so it stays minimal and readable.
        """
        code = r['code']
        max_uses = r.get('max_uses', 0) or 0
        uses = r.get('uses', 0) or 0
        if r.get('expired'):
            if max_uses > 0:
                return f"```diff\n- discord.gg/{code}\n  {uses}/{max_uses} used • EXPIRED\n```"
            return f"```diff\n- discord.gg/{code}\n  EXPIRED\n```"
        if max_uses > 0:
            remaining = max(0, max_uses - uses)
            return (
                f"```\n"
                f"discord.gg/{code}\n"
                f"{uses}/{max_uses} used • {remaining} left\n"
                f"```"
            )
        # Unlimited invites.
        return f"```\ndiscord.gg/{code}\nUnlimited • {uses} used\n```"

    
    async def post_initial_message(self, channel: discord.TextChannel) -> discord.Message:
        embed = discord.Embed(description="***Invite Links:***\nSetting up invite links...", color=discord.Color.gold())
        message = await channel.send(embed=embed)
        self.invite_message_id = message.id
        self.invite_channel_id = channel.id
        self.save_data()
        return message
    
    async def check_and_update_all(self, guild: discord.Guild, channel: discord.TextChannel) -> bool:
        any_changed = False
        for code in list(self.tracked_invites.keys()):
            status = await self.check_invite_status(guild, code)
            if status['expired'] and self.tracked_invites[code]['status'] != 'expired':
                self.tracked_invites[code]['status'] = 'expired'
                self.tracked_invites[code]['expired'] = True
                self.tracked_invites[code]['uses'] = status['max_uses']
                any_changed = True
                logging.info(f"[InviteManager] Invite {code} has expired")
            elif status['valid']:
                self.tracked_invites[code]['uses'] = status['uses']
                if self.tracked_invites[code]['status'] != 'active':
                    self.tracked_invites[code]['status'] = 'active'
                    any_changed = True
        if any_changed:
            self.save_data()
            await self.update_invite_message(channel)
        return any_changed
    
    async def regenerate_expired(self, guild: discord.Guild, channel: discord.TextChannel, archived_by: Optional[int] = None) -> bool:
        if not self.tracked_invites:
            return False
        all_expired = all(data.get('expired', False) or data['status'] == 'expired' for data in self.tracked_invites.values())
        if all_expired:
            logging.info("[InviteManager] All invites expired, regenerating...")
            # Archive the outgoing batch so the "View Previous Invites" button
            # can show it later. Non-fatal if this fails.
            try:
                data_manager.archive_invite_batch(self.tracked_invites, guild.id, archived_by)
            except Exception as e:
                logging.warning(f"[InviteManager] Could not archive expired batch: {e}")
            new_invites = await self.generate_all_invites(guild)
            if not new_invites:
                return False
            for code in self.tracked_invites:
                self.tracked_invites[code]['expired'] = True
                self.tracked_invites[code]['status'] = 'expired'
            for code, data in new_invites.items():
                self.tracked_invites[code] = data
            self.save_data()
            await self.update_invite_message(channel)
            return True
        return False

    def archive_current_batch(self, guild_id: Optional[int], archived_by: Optional[int]) -> int:
        """Public helper so the !regenerateinvites command can archive the
        current batch BEFORE minting new ones (covers the manual-regenerate
        case where invites may not all be expired yet)."""
        return data_manager.archive_invite_batch(self.tracked_invites, guild_id, archived_by)


# --- DATA PERSISTENCE FUNCTIONS ---
def load_blacklist_data() -> None:
    global blacklisted_keywords
    try:
        blacklisted_keywords = data_manager.load_blacklist()
        logging.info(f"[Blacklist] Loaded {len(blacklisted_keywords)} blacklisted keywords from SQLite")
    except Exception as e:
        logging.error(f"[Blacklist] Error loading data: {e}")
        blacklisted_keywords = set()


def save_blacklist_data() -> None:
    try:
        data_manager.save_blacklist(blacklisted_keywords)
    except Exception as e:
        logging.error(f"[Blacklist] Error saving data: {e}")


def load_rules_cache() -> None:
    global rules_cache
    try:
        rules_cache = data_manager.load_rules_cache()
        logging.info("[RulesCache] Loaded cached rules from SQLite")
    except Exception as e:
        logging.error(f"[RulesCache] Error loading: {e}")


def save_rules_cache() -> None:
    try:
        data_manager.save_rules_cache(rules_cache)
    except Exception as e:
        logging.error(f"[RulesCache] Error saving: {e}")


def save_giveaways_data(giveaway_id: Optional[str] = None) -> None:
    """Save giveaways data to SQLite. If giveaway_id is provided, only saves that one."""
    try:
        if giveaway_id:
            g = giveaways_data.get(giveaway_id)
            if g:
                data_manager.save_giveaway(g)
        else:
            for g in giveaways_data.values():
                data_manager.save_giveaway(g)
    except Exception as e:
        logging.error(f"[Giveaways] Error saving data: {e}")

def load_giveaways_data() -> None:
    """Load giveaways data from SQLite so active giveaways survive a restart."""
    global giveaways_data
    try:
        giveaways_data = data_manager.load_all_giveaways()
        logging.info(f"[Giveaways] Loaded {len(giveaways_data)} giveaway(s) from SQLite")
    except Exception as e:
        logging.error(f"[Giveaways] Error loading data, starting fresh: {e}")
        giveaways_data = {}

def reset_giveaways_data() -> None:
    """Clear giveaways data (used on shutdown if desired)."""
    global giveaways_data
    giveaways_data = {}

def save_levels_data() -> None:
    try:
        data_manager.save_all_levels(levels_data)
    except Exception as e:
        logging.error(f"[Levels] Error saving data: {e}")


def load_levels_data() -> None:
    global levels_data
    try:
        levels_data = data_manager.load_all_levels()
        logging.info(f"[Levels] Loaded {len(levels_data)} user levels from SQLite")
    except Exception as e:
        logging.error(f"[Levels] Error loading data: {e}")


def save_all_data() -> None:
    """Save all persistent data to SQLite.

    Tickets persist in SQLite via data_manager (written on every ticket event),
    so there is no separate tickets save step here. Giveaways are intentionally
    in-memory/JSON and reset on restart.
    """
    if invite_manager:
        invite_manager.save_data()
    save_blacklist_data()
    save_rules_cache()
    save_levels_data()
    logging.info("[DataManager] All persistent data saved to SQLite.")


def reset_temporary_data() -> None:
    """Reset temporary in-memory data (giveaways) on bot shutdown.

    Tickets are persisted in SQLite and are intentionally NOT reset here.
    """
    reset_giveaways_data()
    logging.info("[DataManager] Temporary data (giveaways) reset.")


def import_json_to_sqlite() -> None:
    """
    Import existing JSON files into SQLite database.
    This runs once on startup if JSON files are found.
    After import, JSON files are renamed to .bak to prevent re-import.
    """
    imported_something = False
    
    # === IMPORT INVITES ===
    if os.path.exists('invite_data.json'):
        try:
            with open('invite_data.json', 'r') as f:
                data = json.load(f)
            
            message_id = data.get('message_id')
            channel_id = data.get('channel_id')
            tracked_invites = data.get('tracked_invites', {})
            
            data_manager.save_invites(message_id, channel_id, tracked_invites)
            
            # Rename to .bak
            os.rename('invite_data.json', 'invite_data.json.bak')
            
            logging.info(f"[Import] Imported {len(tracked_invites)} invite(s) from invite_data.json")
            imported_something = True
        except Exception as e:
            logging.error(f"[Import] Error importing invites: {e}")
    
    # === IMPORT LEVELS ===
    if os.path.exists('levels_data.json'):
        try:
            with open('levels_data.json', 'r') as f:
                data = json.load(f)
            
            count = 0
            for key, value in data.items():
                parts = key.split('_')
                if len(parts) == 2:
                    user_id = int(parts[0])
                    guild_id = int(parts[1])
                    data_manager.save_level(
                        user_id, guild_id,
                        value.get('xp', 0),
                        value.get('level', 0),
                        value.get('total_messages', 0),
                        value.get('last_xp_gain')
                    )
                    count += 1
            
            # Rename to .bak
            os.rename('levels_data.json', 'levels_data.json.bak')
            
            logging.info(f"[Import] Imported {count} user level(s) from levels_data.json")
            imported_something = True
        except Exception as e:
            logging.error(f"[Import] Error importing levels: {e}")
    
    # === IMPORT RULES CACHE ===
    if os.path.exists('rules_cache.json'):
        try:
            with open('rules_cache.json', 'r') as f:
                data = json.load(f)
            
            data_manager.save_rules_cache(data)
            
            # Rename to .bak
            os.rename('rules_cache.json', 'rules_cache.json.bak')
            
            logging.info(f"[Import] Imported rules cache from rules_cache.json")
            imported_something = True
        except Exception as e:
            logging.error(f"[Import] Error importing rules cache: {e}")
    
    # === IMPORT BLACKLIST ===
    if os.path.exists('blacklist_data.json'):
        try:
            with open('blacklist_data.json', 'r', encoding='utf-8') as f:
                data = json.load(f)
            
            keywords = set(data.get('blacklisted_keywords', []))
            data_manager.save_blacklist(keywords)
            
            # Rename to .bak
            os.rename('blacklist_data.json', 'blacklist_data.json.bak')
            
            logging.info(f"[Import] Imported {len(keywords)} blacklist keyword(s) from blacklist_data.json")
            imported_something = True
        except Exception as e:
            logging.error(f"[Import] Error importing blacklist: {e}")

    # === IMPORT BRANDING ===
    if os.path.exists(config.branding_file):
        try:
            with open(config.branding_file, 'r', encoding='utf-8') as f:
                data = json.load(f)
            data_manager.set_config_value("branding", json.dumps(data))
            os.rename(config.branding_file, f"{config.branding_file}.bak")
            logging.info("[Import] Imported branding config to SQLite")
            imported_something = True
        except Exception as e:
            logging.error(f"[Import] Error importing branding: {e}")

    # === IMPORT CHANNEL CONFIG ===
    if os.path.exists(config.channel_config_file):
        try:
            with open(config.channel_config_file, 'r', encoding='utf-8') as f:
                data = json.load(f)
            data_manager.set_config_value("channels", json.dumps(data))
            os.rename(config.channel_config_file, f"{config.channel_config_file}.bak")
            logging.info("[Import] Imported channel config to SQLite")
            imported_something = True
        except Exception as e:
            logging.error(f"[Import] Error importing channel config: {e}")

    # === IMPORT GIVEAWAYS ===
    if os.path.exists(config.giveaways_data_file):
        try:
            with open(config.giveaways_data_file, 'r', encoding='utf-8') as f:
                data = json.load(f)
            if isinstance(data, dict):
                for g_id, g_data in data.items():
                    g_data['giveaway_id'] = g_id
                    data_manager.save_giveaway(g_data)
                os.rename(config.giveaways_data_file, f"{config.giveaways_data_file}.bak")
                logging.info(f"[Import] Imported {len(data)} giveaway(s) to SQLite")
                imported_something = True
        except Exception as e:
            logging.error(f"[Import] Error importing giveaways: {e}")
    
    if imported_something:
        logging.info("[Import] JSON import complete! Old files renamed to .bak")
    else:
        logging.info("[Import] No JSON files found to import")


# --- BLACKLIST HELPERS ---
def check_text_for_keywords(text: str) -> Tuple[bool, Optional[str]]:
    if not text:
        return (False, None)
    text_lower = text.lower()
    for keyword in blacklisted_keywords:
        if keyword.lower() in text_lower:
            return (True, keyword)
    return (False, None)


async def check_user_profile_for_blacklist(member: discord.Member) -> Tuple[bool, Optional[str], Optional[str]]:
    if not blacklisted_keywords:
        return (False, None, None)
    
    if isinstance(member, discord.Member) and member.activities:
        for activity in member.activities:
            if activity.type == discord.ActivityType.custom:
                parts = []
                if hasattr(activity, 'state') and activity.state:
                    parts.append(activity.state)
                if hasattr(activity, 'name') and activity.name:
                    parts.append(activity.name)
                if hasattr(activity, 'emoji') and activity.emoji and activity.emoji.name:
                    parts.append(activity.emoji.name)
                status_text = ' '.join(parts).strip()
                if status_text:
                    found, keyword = check_text_for_keywords(status_text)
                    if found:
                        return (True, keyword, "Custom Status")
    
    found, keyword = check_text_for_keywords(member.display_name)
    if found:
        return (True, keyword, "Display Name")
    
    found, keyword = check_text_for_keywords(member.name)
    if found:
        return (True, keyword, "Username")
    
    return (False, None, None)


async def auto_ban_if_blacklisted(member: discord.Member, source: str = "unknown") -> bool:
    if not ows_get("auto_ban_profile"):
        return False

    is_blacklisted, keyword, location = await check_user_profile_for_blacklist(member)
    if not is_blacklisted:
        return False

    if ows_get("blacklist_alert_only"):
        logging.info(f"[Blacklist] ALERT ONLY: {member} matched '{keyword}' in {location} (source: {source})")
        log_channel = bot.get_channel(config.channels.log)
        if log_channel:
            embed = discord.Embed(title="⚠️ Blacklist Alert (Review Mode)", color=discord.Color.orange())
            embed.add_field(name="User", value=f"{member.mention} ({member.name})", inline=True)
            embed.add_field(name="Matched Keyword", value=f"**{keyword}**", inline=True)
            embed.add_field(name="Location", value=location, inline=True)
            embed.add_field(name="Triggered By", value=source, inline=True)
            embed.timestamp = datetime.now(timezone.utc)
            await log_channel.send(embed=embed)
        return False

    try:
        await member.ban(reason=f"Auto-banned: Blacklisted keyword '{keyword}' found in {location}")
        logging.info(f"[Blacklist] Auto-banned {member} (ID: {member.id}) via {source} - Keyword '{keyword}' in {location}")
        
        log_channel = bot.get_channel(config.channels.log)
        if log_channel:
            embed = discord.Embed(title="Auto-Ban: Blacklisted Keyword Detected", color=discord.Color.red())
            embed.add_field(name="User", value=f"{member.mention} ({member.name})", inline=True)
            embed.add_field(name="User ID", value=str(member.id), inline=True)
            embed.add_field(name="Matched Keyword", value=f"**{keyword}**", inline=True)
            embed.add_field(name="Location", value=location, inline=True)
            embed.add_field(name="Triggered By", value=source, inline=True)
            embed.timestamp = datetime.now(timezone.utc)
            await log_channel.send(embed=embed)
        return True
    except discord.Forbidden:
        logging.warning(f"[Blacklist] Failed to ban {member} - No permission")
    except discord.HTTPException as e:
        logging.error(f"[Blacklist] Failed to ban {member} - HTTP Error: {e}")
    return False


async def scan_and_ban_blacklisted_members(guild: discord.Guild) -> Tuple[int, int, List[Dict[str, Any]]]:
    banned_count = 0
    failed_count = 0
    matches: List[Dict[str, Any]] = []
    
    for member in guild.members:
        if member.bot:
            continue
        is_blacklisted, keyword, location = await check_user_profile_for_blacklist(member)
        if is_blacklisted:
            matches.append({'user': member, 'keyword': keyword, 'location': location})
            try:
                await member.ban(reason=f"Auto-banned: Blacklisted keyword '{keyword}' found in {location}")
                banned_count += 1
                logging.info(f"[Blacklist] Banned {member} (ID: {member.id}) - Keyword '{keyword}' in {location}")
            except discord.Forbidden:
                failed_count += 1
                logging.warning(f"[Blacklist] Failed to ban {member} - No permission")
            except discord.HTTPException as e:
                failed_count += 1
                logging.error(f"[Blacklist] Failed to ban {member} - HTTP Error: {e}")
    
    return (banned_count, failed_count, matches)


# --- BACKGROUND TASKS ---
@tasks.loop(minutes=config.timing.invite_check_interval_minutes)
async def check_invites_task() -> None:
    global invite_manager
    if not invite_manager or not invite_manager.invite_channel_id:
        return
    channel = invite_manager.bot.get_channel(invite_manager.invite_channel_id)
    if not channel:
        return
    guild = channel.guild
    await invite_manager.check_and_update_all(guild, channel)
    await invite_manager.regenerate_expired(guild, channel)


@check_invites_task.before_loop
async def before_check_invites() -> None:
    global invite_manager
    await bot.wait_until_ready()
    # Wait until invite_manager is actually initialized by on_ready
    while invite_manager is None:
        await asyncio.sleep(1)


@tasks.loop(minutes=config.timing.report_message_interval_minutes)
async def send_report_message() -> None:
    if not messages_enabled:
        return
    channel = bot.get_channel(config.channels.log)
    if channel:
        await channel.send(brand_text(random.choice(REPORT_TEMPLATES)))


@tasks.loop(hours=config.timing.auto_scan_interval_hours)
async def auto_blacklist_scan() -> None:
    if not blacklisted_keywords:
        return
    for guild in bot.guilds:
        log_channel = bot.get_channel(config.channels.auto_scan)
        status_msg = None
        if log_channel:
            status_msg = await log_channel.send("Running scheduled blacklist scan...")
        banned_count, failed_count, matches = await scan_and_ban_blacklisted_members(guild)
        if status_msg:
            try:
                await status_msg.delete()
            except discord.HTTPException:
                pass
        if banned_count > 0 or failed_count > 0:
            if log_channel:
                embed = discord.Embed(title="Scheduled Blacklist Scan Complete", color=discord.Color.red() if banned_count > 0 else discord.Color.orange())
                embed.add_field(name="Members Banned", value=f"**{banned_count}**", inline=True)
                embed.add_field(name="Failed to Ban", value=f"**{failed_count}**", inline=True)
                if matches:
                    match_text = ""
                    for match in matches[:5]:
                        match_text += f"- {match['user'].name} - `{match['keyword']}` in {match['location']}\n"
                    if len(matches) > 5:
                        match_text += f"... and {len(matches) - 5} more"
                    embed.add_field(name="Matches", value=match_text, inline=False)
                embed.set_footer(text=f"Next scan in {config.timing.auto_scan_interval_hours} hours")
                embed.timestamp = datetime.now(timezone.utc)
                result_msg = await log_channel.send(embed=embed)
                await asyncio.sleep(30)
                try:
                    await result_msg.delete()
                except discord.HTTPException:
                    pass


@auto_blacklist_scan.before_loop
async def before_auto_scan() -> None:
    await bot.wait_until_ready()


@tasks.loop(minutes=1)
async def check_giveaways_task() -> None:
    """Check for ended giveaways."""
    if not config.enable_giveaways:
        return
    
    now = datetime.now(timezone.utc)
    to_end = []
    
    for giveaway_id, giveaway in giveaways_data.items():
        if giveaway.get('status') != 'active':
            continue
        try:
            ends_at = datetime.fromisoformat(giveaway['ends_at'])
            if ends_at <= now:
                to_end.append(giveaway_id)
        except:
            pass
    
    for giveaway_id in to_end:
        await end_giveaway(giveaway_id)


async def end_giveaway(giveaway_id: str) -> None:
    """End a giveaway and pick winners.

    Acquires the SAME per-giveaway asyncio.Lock that the entry button uses,
    so a late entry can't be appended to `entries` while we're mid-selection.
    Previously, end_giveaway read the entry list and chose winners without
    the lock, so an in-flight Enter click could append a user AFTER the
    winner list was frozen — that user would be silently excluded.
    """
    if giveaway_id not in giveaways_data:
        return

    # Hold the entry lock for the whole end operation: read entries, pick
    # winners, flip status to 'ended', and persist — all atomically with
    # respect to concurrent Enter clicks.
    lock = _get_giveaway_lock(giveaway_id)
    async with lock:
        if giveaway_id not in giveaways_data:
            return
        giveaway = giveaways_data[giveaway_id]

        # If another caller already ended it, don't double-end (avoids a
        # second winner pick + a duplicate "Giveaway Ended" broadcast).
        if giveaway.get('status') != 'active':
            return

        entries = giveaway.get('entries', [])
        winner_count = giveaway.get('winner_count', 1)

        if entries:
            winners = random.sample(entries, min(winner_count, len(entries)))
            giveaway['winners'] = winners
        else:
            giveaway['winners'] = []

        giveaway['status'] = 'ended'
        giveaway['ended_at'] = datetime.now(timezone.utc).isoformat()
        save_giveaways_data(giveaway_id)  # Save only the updated giveaway to SQLite

        channel = bot.get_channel(giveaway.get('channel_id'))
        if channel:
            winners = giveaway.get('winners', [])
            if winners:
                winners_mention = " ".join(f"<@{wid}>" for wid in winners)
                embed = EmbedBuilder.success(
                    "🎉 Giveaway Ended!",
                    f"**Prize:** {giveaway.get('prize', 'Unknown')}\n"
                    f"**Winners:** {winners_mention}\n"
                    f"**Total Entries:** {len(entries)}"
                )
            else:
                embed = EmbedBuilder.warning(
                    "🎉 Giveaway Ended",
                    f"**Prize:** {giveaway.get('prize', 'Unknown')}\n"
                    f"No valid entries received."
                )

            try:
                message = await channel.fetch_message(giveaway.get('message_id', 0))
                await message.edit(embed=embed, view=None)
            except Exception:
                pass

            await channel.send(embed=embed)

    logging.info(f"[Giveaway] Ended giveaway {giveaway_id}")
    # Release the per-giveaway entry lock now that the giveaway is over.
    _drop_giveaway_lock(giveaway_id)


@check_giveaways_task.before_loop
async def before_check_giveaways() -> None:
    await bot.wait_until_ready()


@tasks.loop(minutes=30)
async def check_sla_task() -> None:
    """Alert in ticket channel if SLA response time has been breached."""
    if not ticket_tool:
        return
    for guild in bot.guilds:
        settings = data_manager.load_ticket_settings(guild.id)
        sla_hours = settings.get('sla_hours', 0) if settings else 0
        if not sla_hours:
            continue
        open_tickets = data_manager.load_tickets_by_guild(guild.id, 'open')
        for ticket in open_tickets:
            if ticket.get('first_response_at'):
                continue  # Already had a staff response
            try:
                created = datetime.fromisoformat(ticket['created_at'].replace('Z', '+00:00'))
                elapsed_hours = (datetime.now(timezone.utc) - created).total_seconds() / 3600
                if elapsed_hours >= sla_hours:
                    channel = guild.get_channel(ticket['channel_id'])
                    if channel:
                        support_role_id = settings.get('support_role_id')
                        mention = f"<@&{support_role_id}>" if support_role_id else "@here"
                        try:
                            await channel.send(
                                f"⚠️ **SLA Breach** — {mention} This ticket has been open for "
                                f"`{elapsed_hours:.1f}h` with no staff response "
                                f"(SLA: {sla_hours}h). Please respond ASAP.",
                                allowed_mentions=discord.AllowedMentions(roles=True, everyone=True)
                            )
                            # Mark as responded so we don't spam
                            data_manager.update_ticket_first_response(ticket['ticket_id'])
                        except Exception:
                            pass
            except Exception as e:
                logging.warning(f"[SLA] Error checking ticket {ticket.get('ticket_id')}: {e}")


@check_sla_task.before_loop
async def before_check_sla() -> None:
    await bot.wait_until_ready()


# --- UTILITY FUNCTIONS ---
def get_uptime() -> str:
    return str(timedelta(seconds=int(time.time() - start_time)))


def log_event(event_type: str, user: discord.User, details: Optional[str] = None) -> None:
    log_message = f"{event_type} - User: {user} (ID: {user.id})"
    if details:
        log_message += f" | Details: {details}"
    logging.info(log_message)


# --- LEVELING SYSTEM (V2 Enhancement) ---
async def process_leveling(message: discord.Message) -> None:
    if not config.enable_leveling or not message.guild:
        return
    
    user_id = message.author.id
    guild_id = message.guild.id
    key = (user_id, guild_id)
    
    # Check cooldown
    level_data = levels_data.get(key, {'xp': 0, 'level': 0, 'total_messages': 0, 'last_xp_gain': None})
    last_gain = level_data.get('last_xp_gain')
    if last_gain:
        try:
            last_time = datetime.fromisoformat(last_gain)
            if (datetime.now(timezone.utc) - last_time).total_seconds() < 60:
                return
        except:
            pass
    
    # Add XP
    xp_gain = random.randint(5, 15)
    level_data['xp'] = level_data.get('xp', 0) + xp_gain
    level_data['total_messages'] = level_data.get('total_messages', 0) + 1
    level_data['last_xp_gain'] = datetime.now(timezone.utc).isoformat()
    
    # Check for level up using the centralized curve helper.
    old_level = level_data.get('level', 0)
    new_level = compute_level_from_xp(level_data['xp'])

    level_data['level'] = new_level
    levels_data[key] = level_data
    
    if new_level > old_level:
        embed = EmbedBuilder.success(
            "🎉 Level Up!",
            f"{message.author.mention} has reached **Level {new_level}**!"
        )
        await message.channel.send(embed=embed, delete_after=10)
    
    # OPTIMIZATION: Save ONLY this user to the database every 10 messages.
    # This prevents the bot from lagging by writing thousands of users at once.
    if level_data['total_messages'] % 10 == 0:
        data_manager.save_level(
            user_id, 
            guild_id, 
            level_data['xp'], 
            level_data['level'], 
            level_data['total_messages'], 
            datetime.fromisoformat(level_data['last_xp_gain']) if level_data.get('last_xp_gain') else None
        )


# --- BOT EVENTS ---
def signal_handler(sig, frame) -> None:
    logging.info("Shutdown signal received. Saving data...")
    save_all_data()
    data_manager.close()
    process_manager.clear_lock_file()
    logging.info("Data saved. Goodbye!")
    sys.exit(0)

signal.signal(signal.SIGINT, signal_handler)
signal.signal(signal.SIGTERM, signal_handler)


# ------------------------------------------------------------------
# FIRST-STARTUP OWNER TUTORIAL
# On the very first launch the bot DMs the application owner a complete
# setup tutorial. A flag file (data/.tutorial_sent) guards it so it only
# fires once. The !tutorial command re-sends it.
# ------------------------------------------------------------------
TUTORIAL_FLAG_FILE = os.path.join(config.data_dir, ".tutorial_sent")


def build_tutorial_embeds() -> List[discord.Embed]:
    """Build the multi-page setup tutorial sent to the owner."""
    gn = brand_text("[GANG NAME]")
    ga = brand_text("[GANG ABBR]")
    embeds: List[discord.Embed] = []

    # --- Page 1: Welcome & Prerequisites ---
    e1 = discord.Embed(
        title=f"🚀 Welcome to Your {gn} Bot — Setup Tutorial",
        description=(
            f"Hello! This is a **one-time** setup tutorial to get your **{gn}** gang bot fully running.\n\n"
            "This guide walks you through every step: branding, channels, roles, verification, and slash commands.\n\n"
            "💬 **Tip:** Run `!tutorial` (or `/tutorial`) at any time to see this guide again."
        ),
        color=discord.Color.green(),
        timestamp=datetime.now(timezone.utc),
    )
    e1.add_field(
        name="✅ Prerequisites (verify these first)",
        value=(
            "1. **Bot invited with both** `bot` **and** `applications.commands` **scopes**\n"
            "   (re-invite with the URL containing `scope=bot applications.commands`)\n"
            "2. **Privileged Gateway Intents enabled** in the Discord Developer Portal:\n"
            "   • Server Members Intent  • Message Content Intent  • Presence Intent\n"
            "3. **Bot has Administrator** (or equivalent) permissions in your server."
        ),
        inline=False,
    )
    e1.add_field(
        name="📚 What this tutorial covers",
        value="`Step 1` Branding  •  `Step 2` In-Server Channels  •  `Step 3` Cross-Server Rules  •  `Step 4` Roles  •  `Step 5` Slash Commands & Final Sync  •  `Page 7` New & Notable Features",
        inline=False,
    )
    e1.set_footer(text="Page 1/7 • First-time setup tutorial")
    embeds.append(e1)

    # --- Page 2: Branding ---
    e2 = discord.Embed(
        title="🎨 Step 1 — Branding",
        description=(
            f"Set your gang identity. Everything the bot says is auto-rebranded from the legacy names to your configured **{gn}** / **{ga}**.\n\n"
            "Run these commands in any channel the bot can see:"
        ),
        color=discord.Color.blurple(),
    )
    e2.add_field(
        name="Set your gang full name",
        value='`!gangname "Brothers Till Death"`\nStops being `[GANG NAME]` once set.',
        inline=False,
    )
    e2.add_field(
        name="Set your gang abbreviation",
        value="`!abrev BTD`\nStops being `[GANG ABBR]` once set (auto-uppercased).",
        inline=False,
    )
    e2.add_field(
        name="Set the bot status",
        value="`!botstatus`\nShows the current status. Edit `config.bot_status` to change what the bot is \"Watching\".",
        inline=False,
    )
    e2.set_footer(text="Page 2/7 • Branding")
    embeds.append(e2)

    # --- Page 3: In-Server Channels ---
    e3 = discord.Embed(
        title="📋 Step 2 — In-Server Channel Setup",
        description=(
            "Run **`!csetup`** for an interactive dropdown menu, OR assign each channel individually with **`!setchannel <type> #channel`**.\n\n"
            "Every channel below MUST be assigned. The names below tell you **exactly** what each one is used for:"
        ),
        color=discord.Color.blurple(),
    )
    e3.add_field(
        name="🟢 Member-facing channels",
        value=(
            "• **Invite Tracker Channel** — tracks invite usage / who invited whom\n"
            "• **Welcome / New-Member Channel** — where new members are greeted\n"
            "• **Server Rules Display Channel** — where your server rules are posted\n"
            "• **Verification Panel Channel** — where the verification button/embed lives (members click here to apply)\n"
            "• **Giveaways Posting Channel** — where giveaways are posted"
        ),
        inline=False,
    )
    e3.add_field(
        name="🟡 Staff / logging channels",
        value=(
            "• **Verification Applications Channel** — where submitted applications are posted for staff review\n"
            "• **User Reports Channel** — where `/report` submissions are sent\n"
            "• **Mod Actions & Broadcasts Log Channel** — logs auto-bans AND sends scheduled broadcast/recruitment messages\n"
            "• **Auto-Blacklist Scan Log Channel** — logs periodic blacklist-scan results"
        ),
        inline=False,
    )
    e3.add_field(
        name="🔴 Tickets system",
        value=(
            "• **Tickets Category** — a Discord **CATEGORY** (not a text channel) where ticket channels are created\n"
            "• **Ticket Transcripts Archive Channel** — where closed-ticket transcripts are saved"
        ),
        inline=False,
    )
    e3.set_footer(text="Page 3/7 • In-Server Channels")
    embeds.append(e3)

    # --- Page 4: Cross-Server Rules Sources ---
    e4 = discord.Embed(
        title="🌐 Step 3 — Cross-Server Rules Sources",
        description=(
            "The bot can display rules from **TWO** Discord servers (the bot must be a member of both).\n"
            "Assign these in `!csetup` or with `!setchannel`:"
        ),
        color=discord.Color.blurple(),
    )
    e4.add_field(
        name="Gang (In-Game) Rules Source Channel",
        value=(
            "Located in **your gang server**. The bot fetches the rules message from this channel automatically.\n"
            "• Refresh with `!updategangrules`\n"
            "• Set manually with `!setgangrules <text>`"
        ),
        inline=False,
    )
    e4.add_field(
        name="Game Server Rules Source Channel",
        value=(
            "Located in the **game server**. The bot fetches the rules message here.\n"
            "• Refresh with `!updateserverrules`\n"
            "• Set manually with `!setserverrules <text>`"
        ),
        inline=False,
    )
    e4.set_footer(text="Page 4/7 • Cross-Server Rules")
    embeds.append(e4)

    # --- Page 5: Roles ---
    e5 = discord.Embed(
        title="👥 Step 4 — Roles",
        description=(
            "The bot references several role IDs for verification, permissions, and moderation.\n"
            "These are pre-configured in the bot and should already match your server. If you need to find your Role IDs easily, run:\n"
            "`!getallroles` or `/getallroles` (Owner only) — Lists all roles and their IDs in a single embed with a Copy List button. Auto-updates when roles change.\n\n"
            "If you need to change them, edit the `RoleConfig` defaults in the bot file:"
        ),
        color=discord.Color.orange(),
    )
    e5.add_field(
        name="Roles used by the bot",
        value=(
            "• **Member** — given to accepted/verified members\n"
            "• **Staff** — moderator/staff role (gated commands)\n"
            "• **Verified** — base verified role\n"
            "• **Muted** — applied by the `!mute` / `!tempmute` commands\n"
            "• **Ticket Support** — who can manage/close tickets\n"
            "• **Server Tester** — tester-only features\n"
            "• **Verification Ping** — pinged when a new application is submitted\n"
            "• **Invite Manager** — invite-management features"
        ),
        inline=False,
    )
    e5.set_footer(text="Page 5/7 • Roles")
    embeds.append(e5)

    # --- Page 6: Final Steps & Slash Commands ---
    e6 = discord.Embed(
        title="✅ Step 5 — Slash Commands & Final Sync",
        description="Almost done! Sync your slash commands so they appear in the `/` menu.",
        color=discord.Color.green(),
    )
    e6.add_field(
        name="Sync slash commands (instant per-guild)",
        value="`!sync YOUR_GUILD_ID`\nCommands appear in the `/` menu within seconds. Get your guild ID with Discord developer mode (right-click your server → Copy ID).",
        inline=False,
    )
    e6.add_field(
        name="Sync globally (slower)",
        value="`!sync`\nPushes to ALL servers — can take up to 1 hour to appear everywhere.",
        inline=False,
    )
    e6.add_field(
        name="Verify it worked",
        value="Type `/` in your server — your bot's commands should appear in the menu.",
        inline=False,
    )
    e6.add_field(
        name="See all commands",
        value="`!cmds`\nLists every prefix + slash command the bot offers.",
        inline=False,
    )
    e6.add_field(
        name="Need this tutorial again?",
        value="`!tutorial`\nRe-sends this guide to your DMs anytime.",
        inline=False,
    )
    e6.set_footer(text=f"Page 6/7 • You're all set! Welcome to {gn}. 🎉")
    embeds.append(e6)

    # --- Page 7: New & Notable Features (today's changes) ---
    e7 = discord.Embed(
        title="🆕 New & Notable Features — What Changed Recently",
        description=(
            "A quick rundown of the most recent improvements so you don't miss them. "
            "Most of these are transparent — they just work better behind the scenes."
        ),
        color=discord.Color.gold(),
        timestamp=datetime.now(timezone.utc),
    )
    e7.add_field(
        name="🧹 `!purge [amount] [-del]` — command message cleanup",
        value=(
            "`!purge 5` deletes 5 messages as before.\n"
            "`!purge 5 -del` (or `/purge amount:5 delete_command:True`) ALSO deletes the `!purge` command message itself and skips the 'Deleted 5 messages.' reply — and the chained-command cleanup prompt is skipped because there's nothing to clean up.\n\n"
            "_Bug fix: purged messages now also appear in the `/msglog` channel — previously they were silently skipped because Discord's bulk-delete doesn't fire on_message_delete._"
        ),
        inline=False,
    )
    e7.add_field(
        name="📋 `/getallroles` — Copy List + auto-updates",
        value=(
            "Owner-only, single embed with all roles & IDs (no pagination).\n"
            "• **Copy List** button — sends the full list to your DMs (ephemeral) for easy copy-paste.\n"
            "• **Auto-updates** — when a role is created, deleted, or renamed, every active `/getallroles` embed in that server refreshes itself.\n"
            "• **Survives restarts** — the button + auto-update still work after the bot comes back online."
        ),
        inline=False,
    )
    e7.add_field(
        name="📨 Invite system — View Previous Invites",
        value=(
            "`/regenerateinvites` now archives the outgoing batch before minting new ones. Click the **📜 View Previous Invites** button on the invite panel to browse old batches, paginated (Previous / Next / Close)."
        ),
        inline=False,
    )
    e7.add_field(
        name="🎫 Ticket system — persistence & race fixes",
        value=(
            "• Ticket panel + control buttons now survive bot restarts (per-panel unique custom_ids).\n"
            "• Claim is atomic (two staff clicking at once can't both win).\n"
            "• Close orders transcript → status update → channel delete, so a crash mid-close no longer leaves a half-closed ticket.\n"
            "• Transcripts pull the entire channel (no more 500-message cap) and escape all HTML so they can't be XSS'd.\n"
            "• Staff notes are actually private (no longer posted to the ticket channel the creator can read)."
        ),
        inline=False,
    )
    e7.add_field(
        name="📊 `/poll` — now hybrid",
        value=(
            "`!poll` and `/poll` both work. Options can be space-separated (`Red Blue Green`) or pipe-separated (`Red|Blue|Green`)."
        ),
        inline=False,
    )
    e7.add_field(
        name="🗄️ Database — giveaways + dbcleanup",
        value=(
            "• Active giveaways now survive bot restarts (loaded from disk instead of reset).\n"
            "• `/dbcleanup` preserves closed-ticket metadata by default (opt-in flag to purge) so historical records aren't accidentally destroyed."
        ),
        inline=False,
    )
    e7.set_footer(text=f"Page 7/7 • {gn} — keeping the streets clean.")
    embeds.append(e7)

    return embeds


async def send_owner_tutorial(force: bool = False) -> None:
    """
    DM the bot application owner the full setup tutorial.

    Fires automatically once on first startup (guarded by a flag file).
    Pass force=True to re-send regardless of the flag (used by !tutorial).
    """
    try:
        if not force and os.path.exists(TUTORIAL_FLAG_FILE):
            return

        # Fetch the application owner from Discord.
        app_info = await bot.application_info()
        owner = app_info.owner
        if owner is None:
            logging.warning("[Tutorial] Could not resolve bot owner; skipping tutorial DM.")
            return

        embeds = build_tutorial_embeds()
        sent = 0
        for embed in embeds:
            try:
                await owner.send(embed=embed)
                sent += 1
            except discord.Forbidden:
                logging.warning("[Tutorial] Owner has DMs closed; cannot send tutorial page %d.", sent + 1)
                break
            except discord.HTTPException as exc:
                logging.warning("[Tutorial] Failed to send tutorial page %d: %s", sent + 1, exc)
                break

        if sent > 0:
            logging.info(f"[Tutorial] Sent {sent}/{len(embeds)} tutorial pages to owner {owner}.")
            print(f"[Tutorial] Sent {sent}/{len(embeds)} tutorial pages to owner {owner}.")
            # Mark as sent so it does not fire again on next startup.
            try:
                with open(TUTORIAL_FLAG_FILE, "w", encoding="utf-8") as fh:
                    fh.write(datetime.now(timezone.utc).isoformat())
            except Exception as exc:
                logging.warning(f"[Tutorial] Could not write flag file: {exc}")
        else:
            logging.warning("[Tutorial] No tutorial pages were delivered. Owner DMs may be closed.")
            print("[Tutorial] WARNING: Could not DM the owner. Run !tutorial in a server to retry.")
    except Exception as exc:
        logging.exception("[Tutorial] Failed to send owner tutorial: %s", exc)
        print(f"[Tutorial] ERROR: {exc}")


@bot.event
async def on_ready() -> None:
    global invite_manager, ticket_tool
    global _on_ready_initialized

    print(f'Logged in as {bot.user.name}')
    print(f'Bot started at: {time.strftime("%Y-%m-%d %H:%M:%S")}')
    logging.info(f'Bot started as {bot.user.name}')

    # Re-set presence on every connect — Discord clears the bot's presence
    # on a gateway reconnect, so this must run each time on_ready fires.
    await bot.change_presence(activity=discord.Activity(type=discord.ActivityType.watching, name=brand_text(config.bot_status)))

    # -----------------------------------------------------------------------
    # RECONNECT PATH: on_ready fires again on every gateway reconnect. The
    # one-time init (DB, caches, slash sync, view registration, task starts)
    # is handled by setup_hook + the _on_ready_initialized guard below. On a
    # reconnect, we only need to re-scan invites (they may have changed while
    # we were disconnected).
    # -----------------------------------------------------------------------
    if _on_ready_initialized:
        if invite_manager:
            try:
                invite_manager.load_data()
                logging.info("[InviteManager] Reconnect: re-scanned invite data")
            except Exception as exc:
                logging.warning(f"[InviteManager] Reconnect rescan failed: {exc}")
        logging.info("[on_ready] Reconnect: one-time init already done (setup_hook); skipping.")
        return

    # -----------------------------------------------------------------------
    # FIRST-CONNECT PATH: everything below runs exactly once.
    # DB connect + cache loads + generic persistent views are already done in
    # setup_hook (which runs before on_ready). This block handles the
    # guild-dependent work that needs bot.guilds to be fully cached.
    # -----------------------------------------------------------------------
    _on_ready_initialized = True

    global _slash_synced
    if not _slash_synced and ows_get("slash_auto_sync"):
        _slash_synced = True
        try:
            synced = await bot.tree.sync()
            logging.info(f"[Slash Commands] Synced {len(synced)} global slash commands")
            print(f"Synced {len(synced)} slash commands")
        except Exception as e:
            logging.exception("[Slash Commands] Failed to sync: %s", e)
            print(f"Failed to sync slash commands: {e}")
    elif not ows_get("slash_auto_sync"):
        logging.info("[Slash Commands] Auto-sync disabled via OWS; skipping")
        _slash_synced = True

    if ows_get("first_startup_tutorial"):
        await send_owner_tutorial(force=False)
    else:
        logging.info("[Tutorial] First-startup tutorial disabled via OWS; skipping")

    process_manager.clear_lock_file()

    if not send_report_message.is_running():
        send_report_message.start()
    invite_manager = InviteManager(bot)
    invite_manager.load_data()

    ticket_tool = TicketToolSystem(data_manager, bot)
    logging.info("[TicketTool] Initialized ticket tool system")

    for guild in bot.guilds:
        guild_panels = data_manager.load_ticket_panels_by_guild(guild.id)
        for panel in guild_panels:
            view = TicketPanelView(panel)
            bot.add_view(view)
    logging.info(f"[TicketTool] Registered views for panels")

    # Re-register GiveawayView for every ACTIVE giveaway so the "Enter
    # Giveaway" button keeps working after a restart. Without this, a
    # giveaway created before the restart would display correctly but its
    # button click would silently no-op (no view handler attached).
    # GiveawayView uses timeout=None (persistent), so add_view is correct.
    if ows_get("enable_giveaways"):
        restored_giveaways = 0
        for giveaway_id, giveaway in giveaways_data.items():
            try:
                if giveaway.get('status') == 'active':
                    bot.add_view(GiveawayView(giveaway_id))
                    restored_giveaways += 1
            except Exception as e:
                logging.warning(f"[Giveaways] Could not re-register view for {giveaway_id}: {e}")
        if restored_giveaways:
            logging.info(f"[Giveaways] Re-registered persistent views for {restored_giveaways} active giveaway(s)")
    else:
        logging.info("[Giveaways] Giveaway system disabled via OWS; skipping view restoration")

    if ows_get("startup_orphan_cleanup"):
        for guild in bot.guilds:
            cleaned = 0
            for status in ('open', 'pending', 'closing'):
                tickets = data_manager.load_tickets_by_guild(guild.id, status)
                for ticket in tickets:
                    channel_id = ticket.get('channel_id')
                    if channel_id is None or not guild.get_channel(channel_id):
                        ticket['status'] = 'closed'
                        ticket['close_reason'] = ticket.get('close_reason') or f'Reconciled on startup (was {status})'
                        if not ticket.get('closed_at'):
                            ticket['closed_at'] = datetime.now(timezone.utc).isoformat()
                        data_manager.save_ticket(ticket)
                        cleaned += 1
            if cleaned:
                logging.info(f"[TicketTool] Reconciled {cleaned} interrupted/orphaned ticket(s) in {guild.name}")
    else:
        logging.info("[TicketTool] Startup orphan cleanup disabled via OWS; skipping")

    if not check_invites_task.is_running():
        check_invites_task.start()
    logging.info("[InviteManager] Started invite checking task")

    gar_refreshed = 0
    gar_pruned = 0
    for guild in bot.guilds:
        tracked = data_manager.load_getallroles_messages(guild_id=guild.id)
        for row in tracked:
            channel = guild.get_channel(row['channel_id'])
            if channel is None:
                data_manager.delete_getallroles_message(row['message_id'])
                gar_pruned += 1
                continue
            try:
                message = await channel.fetch_message(row['message_id'])
            except (discord.NotFound, discord.Forbidden, discord.HTTPException):
                data_manager.delete_getallroles_message(row['message_id'])
                gar_pruned += 1
                continue
            try:
                embed = build_getallroles_embed(guild)
                await message.edit(embed=embed, view=GetAllRolesView())
                gar_refreshed += 1
            except (discord.HTTPException, discord.Forbidden) as e:
                logging.warning(f"[GetAllRoles] Could not refresh message {row['message_id']} on startup: {e}")
    if gar_refreshed or gar_pruned:
        logging.info(f"[GetAllRoles] Startup refresh: {gar_refreshed} refreshed, {gar_pruned} pruned")

    if not auto_blacklist_scan.is_running():
        auto_blacklist_scan.start()
    logging.info("[Blacklist] Started scheduled auto-scan task")

    if config.enable_giveaways and not check_giveaways_task.is_running():
        check_giveaways_task.start()
        logging.info("[Giveaways] Started giveaway check task")

    if not check_sla_task.is_running():
        check_sla_task.start()
        logging.info("[SLA] Started SLA check task")

    await restore_temp_mutes()
    if not check_temp_mutes_task.is_running():
        check_temp_mutes_task.start()
        logging.info("[TempMute] Started temp-mute background check task")

    try:
        pruned = data_manager.prune_message_cache(keep_recent=5000)
        if pruned:
            logging.info(f"[MsgLog] Startup prune removed {pruned} cached message(s)")
        if not prune_message_cache_task.is_running():
            prune_message_cache_task.start()
            logging.info("[MsgLog] Started message-cache prune task")
    except Exception as exc:
        logging.warning(f"[MsgLog] Could not start prune task: {exc}")

    try:
        for g in bot.guilds:
            rr_count = data_manager.count_reaction_roles(g.id)
            sticky_on = StickyRoleSystem.is_enabled(g.id)
            ml_cfg = MessageLogSystem.get_config(g.id)
            branding = data_manager.get_branding(g.id)
            logging.info(
                f"[Premium] {g.name}: RR={rr_count}/250 sticky={'on' if sticky_on else 'off'} "
                f"msglog={'on' if ml_cfg.get('enabled') else 'off'} "
                f"branding_footer={'set' if branding.get('embed_footer') else 'default'}"
            )
    except Exception as exc:
        logging.debug(f"[Premium] startup summary failed: {exc}")

    # =========================================================================
    # RESTORE ACTIVE OWS PANEL (so the owner doesn't have to type !ows again)
    # =========================================================================
    ows_state = data_manager.load_ows_panel_state()
    if ows_state:
        try:
            channel = bot.get_channel(ows_state['channel_id'])
            if channel:
                message = await channel.fetch_message(ows_state['message_id'])
                view = OwnerSettingsView(ows_state['owner_id'])
                view.current_category = ows_state.get('current_category', OWS_CATEGORIES[0])
                view._build_components()
                view.message = message
                await message.edit(view=view)
                logging.info(f"[OWS] Restored active owner settings panel from previous session.")
        except discord.NotFound:
            data_manager.delete_ows_panel_state()
            logging.info("[OWS] Active panel message was deleted, cleared panel state.")
        except Exception as exc:
            logging.warning(f"[OWS] Could not restore active panel: {exc}")

@bot.event
async def on_member_join(member: discord.Member) -> None:
    try:
        banned = await auto_ban_if_blacklisted(member, source="member_join")
        if banned:
            return

        if ows_get("welcome_messages"):
            channel = bot.get_channel(config.channels.welcome)
            welcome_message = brand_text(random.choice(WELCOME_TEMPLATES)).format(mention=member.mention, server=member.guild.name)
            
            embed = discord.Embed(title=f"Welcome to {member.guild.name}!", description=welcome_message, color=discord.Color.green())
            embed.set_thumbnail(url=member.avatar.url if member.avatar else member.default_avatar.url)
            embed.add_field(name="Member Count", value=member.guild.member_count)
            embed.add_field(name="Account Created", value=member.created_at.strftime("%Y-%m-%d"))
            
            rules_channel = bot.get_channel(config.channels.rules)
            if rules_channel:
                embed.add_field(name="Server Rules", value=f"Make sure to follow {config.gang_name}'s rules {rules_channel.mention}", inline=False)
            
            embed.set_footer(text=f"Joined on {member.joined_at.strftime('%Y-%m-%d')}")
            
            welcome_msg = await channel.send(embed=embed)
            await welcome_msg.add_reaction('🔥')
            await welcome_msg.add_reaction('👋')
            await welcome_msg.add_reaction('💯')
            
            logging.info(f"Sent welcome message for {member}")
    except Exception as e:
        logging.error(f"Error in welcome system: {str(e)}")

    # --- Sticky Roles: re-apply saved roles on rejoin (Dyno premium clone) ---
    try:
        restored = await StickyRoleSystem.restore_member_roles(member)
        if restored:
            logging.info(f"[Sticky] Restored {restored} role(s) to returning member {member}")
    except Exception as exc:
        logging.exception(f"[Sticky] restore on join failed: {exc}")


@bot.event
async def on_presence_update(before: discord.Member, after: discord.Member) -> None:
    if after.bot:
        return
    if not blacklisted_keywords:
        return
    
    before_activities = set(str(a) for a in before.activities)
    after_activities = set(str(a) for a in after.activities)
    
    if before_activities == after_activities:
        return
    
    await auto_ban_if_blacklisted(after, source="presence_update")

# =============================================================================
# MULTI-COMMAND CHAINING
# Allows users to run multiple commands in one message, e.g.:
#   !setupinvites, !regenerateinvites
#   !kick @user, !ban @user
#
# Splits on ", !" (comma + optional whitespace + prefix) so commas inside
# command arguments (e.g. !warn @user reason, with comma) are NOT split.
# =============================================================================
MULTI_COMMAND_SPLIT_REGEX = re.compile(
    r'\s*,\s*(?=' + re.escape(config.command_prefix) + r')'
)
MAX_CHAINED_COMMANDS = 10  # Safety cap to prevent abuse

class CommandCleanupView(View):
    """Prompt to ask staff if they want to delete the original chained command message."""
    def __init__(self, original_message: discord.Message):
        super().__init__(timeout=30.0)
        self.original_message = original_message
        self.cleanup_msg = None

    @discord.ui.button(label="Yes, delete it", style=discord.ButtonStyle.danger, emoji="🗑️")
    async def yes_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.original_message.author.id:
            await interaction.response.send_message("This isn't your command to clean up!", ephemeral=True)
            return
        try:
            await self.original_message.delete()
        except (discord.Forbidden, discord.HTTPException, discord.NotFound):
            pass
        # Instead of leaving a "deleted" status message behind, just delete
        # the cleanup prompt so the chat stays clean.
        try:
            await interaction.message.delete()
        except (discord.Forbidden, discord.HTTPException, discord.NotFound):
            # Fallback: if we can't delete it, at least clear the buttons.
            try:
                await interaction.response.edit_message(embed=None, view=None)
            except Exception:
                pass
        self.stop()

    @discord.ui.button(label="No, keep it", style=discord.ButtonStyle.secondary, emoji="✋")
    async def no_button(self, interaction: discord.Interaction, button: Button):
        if interaction.user.id != self.original_message.author.id:
            await interaction.response.send_message("This isn't your command to clean up!", ephemeral=True)
            return
        # Instead of leaving a "kept" status message behind, just delete the
        # cleanup prompt so the chat stays clean.
        try:
            await interaction.message.delete()
        except (discord.Forbidden, discord.HTTPException, discord.NotFound):
            # Fallback: if we can't delete it, at least clear the buttons.
            try:
                await interaction.response.edit_message(embed=None, view=None)
            except Exception:
                pass
        self.stop()

    async def on_timeout(self) -> None:
        # Instead of leaving an "expired" status message behind, just delete
        # the cleanup prompt so the chat stays clean.
        if self.cleanup_msg:
            try:
                await self.cleanup_msg.delete()
            except (discord.Forbidden, discord.HTTPException, discord.NotFound):
                pass

# Global dictionary to temporarily hold page offsets and chain status for chained commands
# Format: {message_id: (page_offset, is_chained)}
_chain_offsets: Dict[int, Tuple[int, bool]] = {}

# =========================================================================
# OWNER SETTINGS HELPERS
# =========================================================================
def get_owner_setting(key: str, default: bool = True) -> bool:
    """Read a boolean owner setting from the SQLite DB."""
    val = data_manager.get_config_value(f"ows_{key}")
    if val is None:
        return default
    return val == "1"

def set_owner_setting(key: str, value: bool) -> None:
    """Save a boolean owner setting to the SQLite DB."""
    data_manager.set_config_value(f"ows_{key}", "1" if value else "0")

# =========================================================================
# !OWS — OWNER SETTINGS (Complete Feature Toggle System)
# =========================================================================
# Discord-native interactive settings panel (NOT a web dashboard).
# Every toggle is persisted to SQLite bot_config via set_owner_setting()
# with the `ows_` prefix. In-memory config.enable_* flags are hydrated
# from the DB on startup via hydrate_ows_settings().
# =========================================================================

@dataclass
class OWSToggle:
    key: str
    label: str
    description: str
    category: str
    default: bool = True
    emoji: str = ""
    apply: Optional[Callable[[bool], None]] = None

OWS_CATEGORIES: List[str] = [
    "🧩 Core Systems",
    "🤖 Automation",
    "⚙️ Moderation",
    "🎫 Tickets",
    "⭐ Premium",
    "🧹 Logging",
    "🚀 Startup/Owner",
    "🛡️ Security",
    "📊 Limits",
    "💬 Branding",
    "🔧 Misc",
]

def _ows_apply_leveling(v: bool) -> None:
    config.enable_leveling = v

def _ows_apply_tickets(v: bool) -> None:
    config.enable_tickets = v

def _ows_apply_giveaways(v: bool) -> None:
    config.enable_giveaways = v
    try:
        if v and not check_giveaways_task.is_running():
            check_giveaways_task.start()
        elif not v and check_giveaways_task.is_running():
            check_giveaways_task.cancel()
    except Exception as exc:
        logging.warning(f"[OWS] giveaway task toggle error: {exc}")

def _ows_apply_warnings(v: bool) -> None:
    config.enable_warnings = v

def _ows_apply_debug(v: bool) -> None:
    config.debug_mode = v

def _ows_apply_periodic(v: bool) -> None:
    global messages_enabled
    messages_enabled = v
    try:
        if v and not send_report_message.is_running():
            send_report_message.start()
        elif not v and send_report_message.is_running():
            send_report_message.stop()
    except Exception as exc:
        logging.warning(f"[OWS] broadcast task toggle error: {exc}")

def _ows_apply_scheduled_scan(v: bool) -> None:
    try:
        if v and not auto_blacklist_scan.is_running():
            auto_blacklist_scan.start()
        elif not v and auto_blacklist_scan.is_running():
            auto_blacklist_scan.cancel()
    except Exception as exc:
        logging.warning(f"[OWS] blacklist scan task toggle error: {exc}")

def _ows_apply_sla(v: bool) -> None:
    try:
        if v and not check_sla_task.is_running():
            check_sla_task.start()
        elif not v and check_sla_task.is_running():
            check_sla_task.cancel()
    except Exception as exc:
        logging.warning(f"[OWS] SLA task toggle error: {exc}")

def _ows_apply_tempmute(v: bool) -> None:
    try:
        if v and not check_temp_mutes_task.is_running():
            check_temp_mutes_task.start()
        elif not v and check_temp_mutes_task.is_running():
            check_temp_mutes_task.cancel()
    except Exception as exc:
        logging.warning(f"[OWS] temp-mute task toggle error: {exc}")

def _ows_apply_msgprune(v: bool) -> None:
    try:
        if v and not prune_message_cache_task.is_running():
            prune_message_cache_task.start()
        elif not v and prune_message_cache_task.is_running():
            prune_message_cache_task.cancel()
    except Exception as exc:
        logging.warning(f"[OWS] msg-cache prune task toggle error: {exc}")

def _ows_apply_invitetask(v: bool) -> None:
    try:
        if v and not check_invites_task.is_running():
            check_invites_task.start()
        elif not v and check_invites_task.is_running():
            check_invites_task.cancel()
    except Exception as exc:
        logging.warning(f"[OWS] invite task toggle error: {exc}")

OWS_TOGGLES: List[OWSToggle] = [
    OWSToggle("enable_leveling",        "Leveling System",       "XP gain, level-ups, /level, /leaderboard",                    "🧩 Core Systems", True,  "📊", _ows_apply_leveling),
    OWSToggle("enable_tickets",         "Tickets System",        "Panels, creation, transcripts, claim/close",                 "🧩 Core Systems", True,  "🎫", _ows_apply_tickets),
    OWSToggle("enable_giveaways",       "Giveaways",             "Creation, entry button, auto-end task",                       "🧩 Core Systems", True,  "🎉", _ows_apply_giveaways),
    OWSToggle("enable_warnings",        "Warnings System",       "/warn, /warnings, auto-ban on threshold",                     "🧩 Core Systems", True,  "⚠️", _ows_apply_warnings),
    OWSToggle("enable_verification",    "Verification System",   "!verify flow (disable for maintenance windows)",              "🧩 Core Systems", True,  "🔐"),
    OWSToggle("enable_invite_tracking","Invite Tracking",       "!setupinvites, tracking panel, auto-regen task",             "🧩 Core Systems", True,  "📨", _ows_apply_invitetask),
    OWSToggle("enable_blacklist_engine","Blacklist Engine",      "Keyword storage + matching engine (core)",                    "🧩 Core Systems", True,  "🚫"),

    OWSToggle("auto_ban_profile",       "Auto-Ban (Profile)",   "Scan name/status/custom status on join & presence update",   "🤖 Automation", True,  "👤"),
    OWSToggle("auto_ban_message",       "Auto-Ban (Message)",   "Scan every guild message's content for keywords",             "🤖 Automation", True,  "💬"),
    OWSToggle("auto_scheduled_scan",   "Scheduled Blacklist Scan","Background scan of all members every 2h",                   "🤖 Automation", True,  "🔄", _ows_apply_scheduled_scan),
    OWSToggle("welcome_messages",       "Welcome Messages",     "Greet new members on join",                                   "🤖 Automation", True,  "👋"),
    OWSToggle("periodic_broadcasts",   "Periodic Broadcasts",  "Scheduled reminder/recruitment messages",                    "🤖 Automation", False, "📢", _ows_apply_periodic),
    OWSToggle("sla_breach_alerts",     "SLA Breach Alerts",    "Ping staff if a ticket has no first response",                "🤖 Automation", True,  "⏰", _ows_apply_sla),
    OWSToggle("temp_mute_loop",         "Temp-Mute Loop",       "Persistent unmute background safety net",                     "🤖 Automation", True,  "🔇", _ows_apply_tempmute),
    OWSToggle("msg_cache_prune",        "Message Cache Prune",  "Keep message-log cache under 5,000 rows",                    "🤖 Automation", True,  "🧹", _ows_apply_msgprune),
    OWSToggle("auto_invite_regen",      "Auto Invite Regen",    "Mint new batch when all invites expire",                      "🤖 Automation", True,  "♻️"),
    OWSToggle("startup_orphan_cleanup","Startup Orphan Cleanup","Auto-close tickets whose channels were deleted while offline","🤖 Automation", True,  "🧹"),

    OWSToggle("multi_command_chain",          "Multi-Command Chaining",   "Parse !cmd1, !cmd2, !cmd3 in one message",          "⚙️ Moderation", True,  "⛓️"),
    OWSToggle("restrict_purge_chain",         "Restrict Purge Chain",     "Block !purge from chaining with !setupinvites etc.", "⚙️ Moderation", True,  "🚫"),
    OWSToggle("command_cleanup_prompt",       "Command Cleanup Prompt",   "Yes/No buttons to delete original command msg",     "⚙️ Moderation", True,  "🧹"),
    OWSToggle("auto_delete_command_del",       "Auto-Delete on -del Flag","Delete command message when -del flag is used",     "⚙️ Moderation", True,  "🗑️"),
    OWSToggle("blacklist_in_tickets",         "Blacklist in Tickets",     "Scan messages inside open ticket channels",         "⚙️ Moderation", False, "🎫"),
    OWSToggle("blacklist_alert_only",         "Blacklist Alert Only",     "Log matches instead of auto-banning (review mode)", "⚙️ Moderation", False, "📢"),
    OWSToggle("enforce_account_age",          "Enforce Account Age",      "Block verification for accounts under 6 days",     "⚙️ Moderation", True,  "📅"),
    OWSToggle("verification_cooldown",        "Verification Cooldown",     "1-hour per-user cooldown on !verify",               "⚙️ Moderation", True,  "⏱️"),

    OWSToggle("require_claim_before_reply",   "Require Claim Before Reply","Staff must claim before responding",              "🎫 Tickets", False, "🙋"),
    OWSToggle("mention_support_on_create",   "Mention Support on Create", "Ping support role when a ticket opens",            "🎫 Tickets", True,  "📌"),
    OWSToggle("dm_transcript_on_close",       "DM Transcript on Close",   "Send transcript DM to creator on close",           "🎫 Tickets", True,  "📬"),
    OWSToggle("auto_close_tickets",           "Auto-Close Idle Tickets", "Close tickets after X hours of inactivity",        "🎫 Tickets", False, "⏲️"),
    OWSToggle("ticket_rating_prompt",         "Ticket Rating Prompt",    "Star rating before close",                         "🎫 Tickets", True,  "⭐"),
    OWSToggle("private_staff_notes",          "Private Staff Notes",     "/note and /notes commands (staff-only)",           "🎫 Tickets", True,  "📝"),
    OWSToggle("ticket_priority_system",       "Ticket Priority System",  "low / normal / high / urgent priority levels",     "🎫 Tickets", True,  "🚨"),
    OWSToggle("ticket_reopen",                "Ticket Reopen",           "Allow reopening closed tickets",                   "🎫 Tickets", True,  "🔓"),
    OWSToggle("ticket_blacklist",              "Ticket Blacklist",       "Block users from creating tickets",                "🎫 Tickets", True,  "🚫"),

    OWSToggle("reaction_roles",         "Reaction Roles",        "Up to 250 per guild (Carl-bot clone)",            "⭐ Premium", True,  "🎨"),
    OWSToggle("sticky_roles",           "Sticky Roles",          "Re-apply roles on rejoin (Dyno clone)",           "⭐ Premium", False, "📌"),
    OWSToggle("full_message_logging",   "Full Message Logging",  "Edit + delete content snapshots",                  "⭐ Premium", False, "📦"),
    OWSToggle("custom_bot_branding",    "Custom Bot Branding",   "Footer / color / thumbnail / image overrides",     "⭐ Premium", True,  "🎨"),

    OWSToggle("log_message_edits",      "Log Message Edits",     "Snapshot edited message content",                 "🧹 Logging", True,  "✏️"),
    OWSToggle("log_message_deletes",    "Log Message Deletes",   "Snapshot deleted message content",                "🧹 Logging", True,  "🗑️"),
    OWSToggle("log_ignore_bots",         "Ignore Bots in Log",    "Skip bot messages in message log",                "🧹 Logging", True,  "🤖"),
    OWSToggle("log_mod_actions",        "Log Mod Actions",       "Bans, kicks, mutes, warns to log channel",        "🧹 Logging", True,  "🔨"),
    OWSToggle("log_auto_bans",           "Log Auto-Bans",         "Log every blacklist auto-ban",                    "🧹 Logging", True,  "⚡"),
    OWSToggle("log_temp_mute_expiry",   "Log Temp-Mute Expiry",  "Log when a temp-mute ends",                       "🧹 Logging", True,  "🔓"),

    OWSToggle("slash_auto_sync",         "Slash Auto-Sync",        "Sync slash commands on every boot",               "🚀 Startup/Owner", True,  "🔄"),
    OWSToggle("first_startup_tutorial",  "First-Startup Tutorial","One-time setup DM to owner",                     "🚀 Startup/Owner", True,  "📩"),
    OWSToggle("debug_mode",              "Debug Mode",            "Enable verbose debug logging",                   "🚀 Startup/Owner", False, "🐞", _ows_apply_debug),
    OWSToggle("force_utf8_output",       "Force UTF-8 Output",    "Reconfigure stdout/stderr to UTF-8 (Windows)",   "🚀 Startup/Owner", True,  "🔤"),

    OWSToggle("account_age_check",          "Account Age Check",       "Min 6 days for verification",                "🛡️ Security", True,  "📅"),
    OWSToggle("security_flags_display",     "Security Flags Display",  "Show 'no avatar', 'short name', etc.",       "🛡️ Security", True,  "🚩"),
    OWSToggle("profile_risk_assessment",    "Profile Risk Assessment", "/securitycheck scoring",                    "🛡️ Security", True,  "📊"),
    OWSToggle("blacklist_ban_threshold",    "Warn-Then-Ban Mode",      "Warn N times then ban (vs instant ban)",     "🛡️ Security", False, "⚠️"),
    OWSToggle("warnings_auto_ban",           "Warnings Auto-Ban",      "Auto-ban when warnings exceed threshold",    "🛡️ Security", True,  "🔨"),

    OWSToggle("enforce_max_tickets",           "Enforce Max Tickets/User",    "Limit tickets per user (config.limits.max_tickets_per_user)",   "📊 Limits", True, "🎫"),
    OWSToggle("enforce_max_giveaway_winners", "Enforce Max Giveaway Winners","Cap winners per giveaway (config.limits.max_giveaway_winners)", "📊 Limits", True, "🎉"),
    OWSToggle("enforce_max_poll_options",      "Enforce Max Poll Options",   "Cap poll options (config.limits.max_poll_options)",              "📊 Limits", True, "📊"),
    OWSToggle("enforce_min_blacklist_len",     "Enforce Min Keyword Length", "Min length for blacklist keywords",                              "📊 Limits", True, "📏"),
    OWSToggle("enforce_verification_timeout",  "Enforce Verification Timeout","Timeout on verification session",                                "📊 Limits", True, "⏱️"),
    OWSToggle("enforce_auto_scan_interval",   "Enforce Auto-Scan Interval",  "2h between scheduled blacklist scans",                           "📊 Limits", True, "🔄"),
    OWSToggle("enforce_invite_check_interval","Enforce Invite Check Interval","5min between invite status checks",                              "📊 Limits", True, "📨"),

    OWSToggle("use_gang_name",              "Use Gang Name",            "Apply gang name in brand_text()",            "💬 Branding", True,  "🏷️"),
    OWSToggle("use_gang_abbreviation",     "Use Gang Abbreviation",    "Apply abbreviation in brand_text()",         "💬 Branding", True,  "🏷️"),
    OWSToggle("use_bot_status",             "Use Custom Bot Status",   "Show custom 'Watching' status",              "💬 Branding", True,  "👀"),
    OWSToggle("use_embed_footer_override", "Embed Footer Override",   "Use per-guild custom embed footer",          "💬 Branding", False, "📄"),
    OWSToggle("use_embed_color_override",  "Embed Color Override",    "Use per-guild custom embed color",           "💬 Branding", False, "🎨"),
    OWSToggle("use_embed_thumbnail_override","Embed Thumbnail Override","Use per-guild custom embed thumbnail",     "💬 Branding", False, "🖼️"),

    OWSToggle("process_lock_file",           "Process Lock File",       "Prevent concurrent bot instances (bot_busy.lock)", "🔧 Misc", True,  "🔒"),
    OWSToggle("persistent_views",            "Persistent Views",        "Re-attach buttons to old messages on restart",     "🔧 Misc", True,  "🔁"),
    OWSToggle("json_to_sqlite_import",       "JSON → SQLite Import",   "One-time import on first launch",                   "🔧 Misc", True,  "📦"),
    OWSToggle("multi_command_max_chain",     "Max Chain Length (10)",  "Enforce MAX_CHAINED_COMMANDS cap",                 "🔧 Misc", True,  "⛓️"),
]

_OWS_TOGGLE_MAP: Dict[str, OWSToggle] = {t.key: t for t in OWS_TOGGLES}

def ows_get(key: str) -> bool:
    """Read an OWS toggle from the DB, applying the toggle's default if unset."""
    t = _OWS_TOGGLE_MAP.get(key)
    default = t.default if t else True
    return get_owner_setting(key, default)

def ows_set(key: str, value: bool) -> None:
    """Persist an OWS toggle to the DB and fire its apply callback immediately."""
    set_owner_setting(key, value)
    t = _OWS_TOGGLE_MAP.get(key)
    if t and t.apply:
        try:
            t.apply(value)
        except Exception as exc:
            logging.warning(f"[OWS] apply callback for '{key}' failed: {exc}")

def hydrate_ows_settings() -> None:
    """Load every OWS toggle from the DB and apply to in-memory state."""
    for t in OWS_TOGGLES:
        val = get_owner_setting(t.key, t.default)
        if t.apply:
            try:
                t.apply(val)
            except Exception as exc:
                logging.warning(f"[OWS] hydrate '{t.key}' failed: {exc}")
    total = len(OWS_TOGGLES)
    enabled = sum(1 for t in OWS_TOGGLES if get_owner_setting(t.key, t.default))
    logging.info(f"[OWS] Hydrated {total} owner settings — {enabled}/{total} enabled")

def ows_bulk_set_category(category: str, value: bool) -> int:
    """Enable or disable every toggle in a category. Returns count changed."""
    changed = 0
    for t in OWS_TOGGLES:
        if t.category == category:
            ows_set(t.key, value)
            changed += 1
    return changed

class OwnerSettingsView(View):
    MAX_TOGGLE_BUTTONS = 15

    def __init__(self, author_id: int):
        super().__init__(timeout=300)
        self.author_id = author_id
        self.current_category: str = OWS_CATEGORIES[0]
        self.message: Optional[discord.Message] = None
        self._build_components()

    def _build_components(self) -> None:
        self.clear_items()
        cat_select = discord.ui.Select(
            placeholder="📋 Select a category to configure…",
            min_values=1,
            max_values=1,
            options=[
                discord.SelectOption(
                    label=cat,
                    value=cat,
                    description=f"{sum(1 for t in OWS_TOGGLES if t.category == cat)} toggle(s)",
                )
                for cat in OWS_CATEGORIES
            ],
            row=0,
        )
        cat_select.callback = self._on_category_selected
        self.add_item(cat_select)

        toggles = self._toggles_for_category()
        for idx, t in enumerate(toggles[:self.MAX_TOGGLE_BUTTONS]):
            row = 1 + (idx // 5)
            if row > 3:
                break
            enabled = ows_get(t.key)
            icon = "✅" if enabled else "❌"
            label = f"{icon} {t.label}"[:80]
            style = discord.ButtonStyle.success if enabled else discord.ButtonStyle.secondary
            btn = Button(label=label, style=style, row=row)
            btn.callback = self._make_toggle_callback(t.key)
            self.add_item(btn)

        enable_all = Button(label="Enable All", style=discord.ButtonStyle.success, row=4, emoji="🔓")
        enable_all.callback = self._on_enable_all
        self.add_item(enable_all)

        disable_all = Button(label="Disable All", style=discord.ButtonStyle.secondary, row=4, emoji="🔒")
        disable_all.callback = self._on_disable_all
        self.add_item(disable_all)

        close_btn = Button(label="Close", style=discord.ButtonStyle.danger, row=4, emoji="✖")
        close_btn.callback = self._on_close
        self.add_item(close_btn)

    def _toggles_for_category(self) -> List[OWSToggle]:
        return [t for t in OWS_TOGGLES if t.category == self.current_category]

    def _make_toggle_callback(self, key: str):
        async def _callback(interaction: discord.Interaction) -> None:
            if interaction.user.id != self.author_id:
                await interaction.response.send_message("❌ This isn't your settings panel!", ephemeral=True)
                return
            new_val = not ows_get(key)
            ows_set(key, new_val)
            logging.info(f"[OWS] {interaction.user} toggled '{key}' → {'ON' if new_val else 'OFF'}")
            self._build_components()
            await interaction.response.edit_message(embed=self._build_embed(), view=self)
        return _callback

    async def _on_category_selected(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("❌ This isn't your settings panel!", ephemeral=True)
            return
        self.current_category = self.category_select_placeholder_values(interaction)
        self._build_components()
        await interaction.response.edit_message(embed=self._build_embed(), view=self)
        # Save the new category so it survives a restart
        data_manager.save_ows_panel_state(self.message.id, self.message.channel.id, self.author_id, self.current_category)

    def category_select_placeholder_values(self, interaction: discord.Interaction) -> str:
        for child in self.children:
            if isinstance(child, discord.ui.Select):
                if child.values:
                    return child.values[0]
        return self.current_category

    async def _on_enable_all(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("❌ This isn't your settings panel!", ephemeral=True)
            return
        count = ows_bulk_set_category(self.current_category, True)
        logging.info(f"[OWS] {interaction.user} enabled all {count} toggles in '{self.current_category}'")
        self._build_components()
        await interaction.response.edit_message(embed=self._build_embed(), view=self)

    async def _on_disable_all(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("❌ This isn't your settings panel!", ephemeral=True)
            return
        count = ows_bulk_set_category(self.current_category, False)
        logging.info(f"[OWS] {interaction.user} disabled all {count} toggles in '{self.current_category}'")
        self._build_components()
        await interaction.response.edit_message(embed=self._build_embed(), view=self)

    async def _on_close(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("❌ This isn't your settings panel!", ephemeral=True)
            return
        self.stop()
        data_manager.delete_ows_panel_state() # Clean up DB on close
        try:
            await interaction.message.delete()
        except (discord.HTTPException, discord.Forbidden, discord.NotFound):
            for child in self.children:
                child.disabled = True
            try:
                await interaction.response.edit_message(view=self)
            except Exception:
                pass

    def _build_embed(self) -> discord.Embed:
        toggles = self._toggles_for_category()
        enabled_count = sum(1 for t in toggles if ows_get(t.key))
        total = len(toggles)

        embed = discord.Embed(
            title=f"⚙️ Owner Settings — {self.current_category}",
            description=(
                f"**{enabled_count}/{total}** features enabled in this category.\n"
                f"Click a button to flip it. Every change is saved to the database instantly.\n"
            ),
            color=discord.Color.blurple() if enabled_count >= total // 2 else discord.Color.orange(),
            timestamp=datetime.now(timezone.utc),
        )

        lines: List[str] = []
        for t in toggles:
            val = ows_get(t.key)
            icon = "🟢" if val else "🔴"
            lines.append(f"{icon} **{t.label}**\n   ↳ {t.description}")

        chunk_size = 4
        for i in range(0, len(lines), chunk_size):
            chunk = lines[i:i + chunk_size]
            field_num = (i // chunk_size) + 1
            field_name = "Features" if field_num == 1 else f"Features (cont. {i + 1}–{i + len(chunk)})"
            embed.add_field(name=field_name, value="\n".join(chunk), inline=False)

        pct = (enabled_count / total * 100) if total else 0
        filled = int(pct / 10)
        bar = "█" * filled + "░" * (10 - filled)
        embed.add_field(
            name="📊 Category Progress",
            value=f"`{bar}` **{pct:.0f}%** ({enabled_count}/{total})",
            inline=False,
        )

        all_enabled = sum(1 for t in OWS_TOGGLES if ows_get(t.key))
        embed.set_footer(
            text=f"Overall: {all_enabled}/{len(OWS_TOGGLES)} features enabled • "
                 f"Use the dropdown to switch categories • Owner-only"
        )
        return embed

    async def on_timeout(self) -> None:
        for child in self.children:
            child.disabled = True
        data_manager.delete_ows_panel_state() # Clean up DB on timeout
        try:
            if self.message:
                await self.message.edit(view=self)
        except Exception:
            pass

@bot.hybrid_command(name="ows", aliases=["OWS", "Ows", "ownerws", "ownersettings"])
@commands.is_owner()
async def ows_cmd(ctx: commands.Context) -> None:
    """Open the Owner Settings panel — toggle any bot feature on or off."""
    view = OwnerSettingsView(ctx.author.id)
    embed = view._build_embed()
    view.message = await ctx.send(embed=embed, view=view)
    # Save the panel state to the database
    data_manager.save_ows_panel_state(view.message.id, ctx.channel.id, ctx.author.id, view.current_category)
    logging.info(f"[OWS] Owner settings panel opened by {ctx.author}")

async def process_potential_multi_command(message: discord.Message) -> None:
    content = message.content
    prefix = config.command_prefix

    if content.startswith(f"{prefix}kick ") or content.startswith(f"{prefix}ban "):
        parts_initial = content.split(" ", 1)
        if len(parts_initial) > 1:
            parts_initial[1] = parts_initial[1].replace(",", " ")
            content = " ".join(parts_initial)
            message.content = content

    if not content.startswith(prefix):
        await bot.process_commands(message)
        return

    parts = MULTI_COMMAND_SPLIT_REGEX.split(content)

    if len(parts) <= 1 or not ows_get("multi_command_chain"):
        await bot.process_commands(message)
        return

    if len(parts) > MAX_CHAINED_COMMANDS:
        try:
            await message.reply(
                f"⚠️ Too many commands chained (max {MAX_CHAINED_COMMANDS}). "
                f"Please split them into separate messages.",
                delete_after=10,
            )
        except discord.HTTPException:
            pass
        return

    normalized_content = content.strip()
    if ' -del' in normalized_content and not normalized_content.lower().endswith('-del'):
        try:
            await message.reply(
                "⚠️ **Invalid Command Format**\n"
                "You cannot use `-del` in the middle of a multi-command chain.\n"
                "If you want to delete the command message, please put `-del` at the **very end** of the entire chain.\n\n"
                "**Correct Format:** `!purgeall, !cmds, !cmds, !cmds, !cmds, !cmds, !setupinvites, !regenerateinvites -del`",
                delete_after=30
            )
        except discord.HTTPException:
            pass
        return

    global_del_flag = False
    if parts[-1].strip().lower().endswith('-del'):
        global_del_flag = True
        parts[-1] = parts[-1].strip()[:-4].strip()

    cmd_names = []
    for part in parts:
        part = part.strip()
        if part.endswith(','):
            part = part[:-1].strip()
        if part.startswith(prefix):
            cmd_names.append(part[len(prefix):].split(" ")[0].lower())
            
    restrict_purge = get_owner_setting("restrict_purge_chain", True)
    if restrict_purge:
        has_purge = any(name in ['purge', 'purgeall'] for name in cmd_names)
        has_restricted = any(name in ['cmds', 'setupinvites', 'regenerateinvites'] for name in cmd_names)
        
        if has_purge and has_restricted:
            try:
                await message.reply(
                    "⚠️ **Invalid Command Format**\n"
                    "You cannot chain `!purge` or `!purgeall` with `!cmds`, `!setupinvites`, or `!regenerateinvites`. \n"
                    "Please run it separately, or ask the owner to disable this restriction with `!ows`.",
                    delete_after=30
                )
            except discord.HTTPException:
                pass
            return

    chain_command_counts = {}
    is_chained = len(cmd_names) > 1

    for idx, part in enumerate(parts, 1):
        part = part.strip()
        
        if part.endswith(','):
            part = part[:-1].strip()
            
        if not part or not part.startswith(prefix):
            continue

        msg_copy = copy.copy(message)
        msg_copy.content = part

        cmd_name = part[len(prefix):].split(" ")[0].lower()
        chain_command_counts[cmd_name] = chain_command_counts.get(cmd_name, 0) + 1
        _chain_offsets[msg_copy.id] = (chain_command_counts[cmd_name] - 1, is_chained)

        try:
            await bot.process_commands(msg_copy)
        except Exception as exc:
            logging.error(f"[MultiCommand] Error running command {idx}/{len(parts)} ('{part}'): {exc}")
            try:
                await message.reply(f"⚠️ Command `{part}` failed: `{exc}`", delete_after=15)
            except discord.HTTPException:
                pass

    is_staff = False
    if message.guild:
        if message.author.guild_permissions.manage_messages or message.author.guild_permissions.administrator:
            is_staff = True

    if global_del_flag and ows_get("auto_delete_command_del"):
        try:
            await message.delete()
        except (discord.HTTPException, discord.Forbidden, discord.NotFound):
            pass
        return

    command_message_still_exists = True
    try:
        if message.channel is not None:
            try:
                await message.channel.fetch_message(message.id)
            except (discord.NotFound, discord.HTTPException):
                command_message_still_exists = False
    except Exception:
        pass

    if is_staff and command_message_still_exists and ows_get("command_cleanup_prompt"):
        cleanup_embed = discord.Embed(
            title="🧹 Cleanup Command Message?",
            description="Do you want to delete the original command message to keep chat clean?",
            color=discord.Color.blurple(),
            timestamp=datetime.now(timezone.utc)
        )
        original_content_preview = message.content[:1024]
        cleanup_embed.add_field(name="Original Message", value=f"> {original_content_preview}", inline=False)
        
        view = CommandCleanupView(message)
        view.cleanup_msg = await message.channel.send(embed=cleanup_embed, view=view)

@bot.event
async def on_message(message: discord.Message) -> None:
    if message.author.bot:
        return

    # Verification channel message counter: increment and persist per-message.
    try:
        # Only count messages in configured verification main channel
        if message.channel and getattr(config, 'channels', None) and message.channel.id == config.channels.verification_main:
            # Load current count from DB (stored as integer string), default 0
            cnt_val = data_manager.get_config_value('verification_msg_count')
            try:
                cnt = int(cnt_val) if cnt_val is not None else 0
            except Exception:
                cnt = 0
            cnt += 1
            data_manager.set_config_value('verification_msg_count', str(cnt))
            # When every 25 messages, send separator line
            if cnt % 25 == 0:
                try:
                    await message.channel.send("--------------------------------------------------------------------------------------------------------------------------------------------------------------------------")
                except Exception:
                    logging.exception("[VerificationCounter] Failed to send separator message")
    except Exception:
        logging.exception("[VerificationCounter] Error updating verification message count")

    open_ticket = None
    if message.guild and ticket_tool:
        try:
            ticket = data_manager.load_ticket_by_channel(message.channel.id)
            if ticket and ticket.get('status') == 'open':
                open_ticket = ticket
        except Exception as e:
            logging.warning(f"[on_message] Ticket lookup failed: {e}")

    if message.guild and blacklisted_keywords and open_ticket is None and ows_get("auto_ban_message"):
        content = message.content or ""
        if not content.startswith(config.command_prefix):
            found, keyword = check_text_for_keywords(content)
            if found:
                author = message.author
                try:
                    await message.delete()
                except (discord.Forbidden, discord.HTTPException, discord.NotFound):
                    pass

                log_channel = bot.get_channel(config.channels.log)
                snippet = content if len(content) <= 900 else (content[:900] + "...")
                if log_channel:
                    try:
                        embed = discord.Embed(
                            title="🚨 Blacklisted Keyword in Message",
                            description=(
                                f"**User:** {author.mention} (`{author.name}` / `{author.id}`)\n"
                                f"**Channel:** {message.channel.mention}\n"
                                f"**Matched Keyword:** `{keyword}`"
                            ),
                            color=discord.Color.red(),
                            timestamp=datetime.now(timezone.utc),
                        )
                        embed.add_field(name="Message Content", value=snippet, inline=False)
                        await log_channel.send(embed=embed)
                    except Exception as e:
                        logging.warning(f"[Blacklist] Could not send message blacklist log: {e}")

                logging.info(
                    f"[Blacklist] Deleted message from {author} (ID: {author.id}) "
                    f"in #{getattr(message.channel, 'name', '?')} containing '{keyword}'"
                )

                if not ows_get("blacklist_alert_only"):
                    if isinstance(author, discord.Member):
                        try:
                            await author.ban(
                                reason=f"Auto-banned: Blacklisted keyword '{keyword}' in message",
                            )
                        except discord.Forbidden:
                            logging.warning(f"[Blacklist] No permission to ban {author}.")
                        except discord.HTTPException as e:
                            logging.error(f"[Blacklist] HTTP error banning {author}: {e}")
                return

    await process_leveling(message)

    try:
        if message.guild is not None:
            MessageLogSystem.cache(message)
    except Exception as exc:
        logging.debug(f"[MsgLog] on_message cache failed: {exc}")

    if open_ticket is not None:
        is_staff_msg = message.author.id != open_ticket.get('creator_id')
        if is_staff_msg and ows_get("require_claim_before_reply"):
            # Exempt command messages (e.g. !claim, !close, !unclaim) so staff
            # can still MANAGE the ticket — the gate applies to conversational
            # replies only, matching the toggle's "Staff must claim before
            # responding" description.
            is_command = (message.content or '').startswith(config.command_prefix)
            if not is_command:
                claimed_by = open_ticket.get('claimed_by')
                if not claimed_by:
                    # Ticket is not claimed — staff must claim before replying.
                    try:
                        await message.delete()
                    except (discord.HTTPException, discord.Forbidden, discord.NotFound):
                        pass
                    try:
                        await message.channel.send(
                            f"{message.author.mention} You must claim this ticket before replying. "
                            f"Click the **Claim** button (or use `{config.command_prefix}claim`).",
                            delete_after=15,
                        )
                    except (discord.HTTPException, discord.Forbidden):
                        pass
                    return
                if claimed_by != message.author.id:
                    # Claimed by another staff member — only the claimer may reply.
                    try:
                        await message.delete()
                    except (discord.HTTPException, discord.Forbidden, discord.NotFound):
                        pass
                    try:
                        await message.channel.send(
                            f"{message.author.mention} This ticket is claimed by <@{claimed_by}>. "
                            f"Only the claimer may reply. Ask them to `{config.command_prefix}unclaim` for a hand-off.",
                            delete_after=15,
                        )
                    except (discord.HTTPException, discord.Forbidden):
                        pass
                    return
        if is_staff_msg:
            data_manager.update_ticket_first_response(open_ticket['ticket_id'])

        # Persist this message to the ticket_messages table so it serves as a
        # transcript BACKUP. The transcript generator still reads Discord
        # channel history as the primary source, but falls back to this table
        # if the channel history is empty/unavailable (e.g. messages were
        # bulk-deleted, or the channel was partially lost before close).
        try:
            await data_manager.async_save_ticket_message({
                'message_id': message.id,
                'ticket_id': open_ticket['ticket_id'],
                'author_id': message.author.id,
                'author_name': message.author.display_name,
                'author_avatar': str(message.author.avatar.url) if message.author.avatar else str(message.author.default_avatar.url),
                'content': message.content or '',
                'attachments': json.dumps([att.url for att in message.attachments]),
                'created_at': message.created_at.isoformat() if message.created_at else datetime.now(timezone.utc).isoformat(),
            })
        except Exception as exc:
            logging.debug(f"[TicketMsg] could not persist ticket message {message.id}: {exc}")

    await process_potential_multi_command(message)


# --- OWNER BRANDING AND CHANNEL SETUP COMMANDS ---
@bot.hybrid_command(name="abrev", aliases=["Abrev", "ABREV"])
@commands.is_owner()
async def abbrev_cmd(ctx: commands.Context, abbreviation: str) -> None:
    value = abbreviation.strip()
    if not value:
        await ctx.send("Usage: `!abrev BTD`")
        return

    config.gang_abbreviation = value.upper()
    config.save_branding_settings()
    await ctx.send(embed=EmbedBuilder.success("Gang Abbreviation Updated", f"Abbreviation set to **{config.gang_abbreviation}**"))


@bot.hybrid_command(name="gangname", aliases=["GangName", "GANGNAME"])
@commands.is_owner()
async def gangname_cmd(ctx: commands.Context, *, name: str) -> None:
    value = name.strip()
    if not value:
        await ctx.send("Usage: `!gangname \"Brothers Till Death\"`")
        return

    config.gang_name = value
    config.bot_status = f"{config.gang_name} On Top"
    config.save_branding_settings()
    await ctx.send(embed=EmbedBuilder.success("Gang Name Updated", f"Gang name set to **{config.gang_name}**"))


@bot.hybrid_command(name="setchannel")
@commands.is_owner()
async def setchannel_cmd(ctx: commands.Context, channel_type: str, channel: discord.TextChannel) -> None:
    valid_types = {
        "welcome": "welcome",
        "rules": "rules",
        "verification": "verification_main",
        "verify": "verification_main",
        "reports": "reports",
        "tickets": "tickets",
        "log": "log",
        "logs": "log",
        "giveaways": "giveaways",
        "auto_scan": "auto_scan",
    }
    key = valid_types.get(channel_type.lower())
    if not key:
        await ctx.send("Usage: `!setchannel <welcome|rules|verification|reports|tickets|log|giveaways> #channel`")
        return

    setattr(config.channels, key, channel.id)
    config.save_channel_settings()
    await ctx.send(embed=EmbedBuilder.success("Channel Updated", f"Set **{channel_type.lower()}** to {channel.mention}"))


# --- CHANNEL SETUP COMMAND (!CSetup) ---
# Interactive dropdown-based configuration for every channel/category the bot uses.
# Replaces the need to hand-edit IDs in ChannelConfig / ServerConfig.
#
# Each entry: (target_attr, attr_name, label, channel_types, guild_source)
#   target_attr   -> "channels" (config.channels) or "servers" (config.servers)
#   attr_name     -> attribute name holding the channel/category ID
#   channel_types -> which discord.ChannelType values to list when assigning
#   guild_source  -> "current" (ctx.guild), "gang", or "server"
CHANNEL_SETUP_SLOTS: List[Tuple[str, str, str, List[discord.ChannelType], str]] = [
    ("channels", "Invite Setup", "Invite Tracker Channel", [discord.ChannelType.text], "current"),
    ("channels", "Welcome Channel", "Welcome / New-Member Channel", [discord.ChannelType.text], "current"),
    ("channels", "Rules Channel", "Server Rules Display Channel", [discord.ChannelType.text], "current"),
    ("channels", "Log Channel", "Mod Actions & Broadcasts Log Channel", [discord.ChannelType.text], "current"),
    ("channels", "Blacklist Scan Channel", "Auto-Blacklist Scan Log Channel", [discord.ChannelType.text], "current"),
    ("channels", "Verify Channel", "People Start their verification app here", [discord.ChannelType.text], "current"),
    ("channels", "verification Submissions", "Staff views new user apps here", [discord.ChannelType.text], "current"),
    ("channels", "reports", "User Reports Channel", [discord.ChannelType.text], "current"),
    ("channels", "tickets", "Tickets Category (Category, NOT a Channel)", [discord.ChannelType.category], "current"),
    ("channels", "transcripts", "Ticket Transcripts Archive Channel", [discord.ChannelType.text], "current"),
    ("channels", "giveaways", "Giveaways Posting Channel", [discord.ChannelType.text], "current"),
    ("servers", "faction_rules_channel", "Gang (In-Game) Rules Source Channel", [discord.ChannelType.text], "gang"),
    ("servers", "server_rules_channel", "Game Server Rules Source Channel", [discord.ChannelType.text], "server"),
]


def _csetup_target(target_attr: str):
    """Return the config sub-object that holds the given slot."""
    return config.channels if target_attr == "channels" else config.servers


def _csetup_guild(guild_source: str, ctx_guild: discord.Guild) -> Optional[discord.Guild]:
    """Resolve which guild's channels to list for a given slot."""
    if guild_source == "current":
        return ctx_guild
    if guild_source == "gang":
        return bot.get_guild(config.servers.gang_server_id)
    if guild_source == "server":
        return bot.get_guild(config.servers.server_server_id)
    return None


def _csetup_main_embed() -> discord.Embed:
    """Build the main Channel Setup embed showing every slot's current value."""
    embed = discord.Embed(
        title="🔧 Channel Setup",
        description=(
            "Use the dropdown below to configure every channel the bot uses.\n"
            "1) Pick a slot to configure.\n"
            "2) Choose the Discord channel to assign.\n"
            "Settings are saved automatically to `channel_config.json`."
        ),
        color=discord.Color.blurple(),
        timestamp=datetime.now(timezone.utc),
    )
    lines = []
    for target_attr, attr_name, label, _, _ in CHANNEL_SETUP_SLOTS:
        cid = getattr(_csetup_target(target_attr), attr_name, 0)
        value = f"<#{cid}>" if cid else "`Not set`"
        lines.append(f"**{label}:** {value}")
    embed.add_field(name="Current Channels", value="\n".join(lines), inline=False)
    embed.set_footer(text="Only the user who started !CSetup can interact.")
    return embed


class ChannelSetupView(View):
    """Step 1: dropdown listing every configurable channel slot."""

    def __init__(self, ctx: commands.Context):
        super().__init__(timeout=300)
        self.ctx = ctx
        self.author_id = ctx.author.id
        self.message: Optional[discord.Message] = None

        options = []
        for target_attr, attr_name, label, _, _ in CHANNEL_SETUP_SLOTS:
            cid = getattr(_csetup_target(target_attr), attr_name, 0)
            current_str = f"Current: {cid}" if cid else "Not set"
            options.append(discord.SelectOption(
                label=label[:100],
                value=f"{target_attr}:{attr_name}",
                description=current_str[:100],
            ))

        self.slot_select = discord.ui.Select(
            placeholder="Select a channel to configure…",
            min_values=1,
            max_values=1,
            options=options,
        )
        self.slot_select.callback = self._on_slot_selected
        self.add_item(self.slot_select)

    async def _on_slot_selected(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("This setup menu is not for you.", ephemeral=True)
            return

        value = self.slot_select.values[0]
        target_attr, attr_name = value.split(":", 1)
        descriptor = next((d for d in CHANNEL_SETUP_SLOTS if d[0] == target_attr and d[1] == attr_name), None)
        if not descriptor:
            await interaction.response.send_message("Invalid selection.", ephemeral=True)
            return

        _, _, label, channel_types, guild_source = descriptor
        guild = _csetup_guild(guild_source, self.ctx.guild)
        if not guild:
            await interaction.response.send_message(
                embed=EmbedBuilder.error("Guild Not Found", "Could not find the target guild for this slot."),
                ephemeral=True,
            )
            return

        channels = sorted(
            [c for c in guild.channels if c.type in channel_types],
            key=lambda c: c.name.lower(),
        )
        if not channels:
            await interaction.response.send_message(
                embed=EmbedBuilder.error("No Channels", f"No matching channels found in **{guild.name}**."),
                ephemeral=True,
            )
            return

        assign_view = ChannelAssignView(
            author_id=self.author_id,
            target_attr=target_attr,
            attr_name=attr_name,
            slot_label=label,
            channels=channels,
            guild_name=guild.name,
            original_view=self,
        )
        await interaction.response.edit_message(embed=assign_view.render_embed(), view=assign_view)

    async def on_timeout(self) -> None:
        for child in self.children:
            child.disabled = True
        try:
            if getattr(self, "message", None) is not None:
                await self.message.edit(view=self)
        except Exception:
            pass


class ChannelAssignView(View):
    """Step 2: paginated dropdown of real Discord channels to assign to the chosen slot.

    Discord caps each select menu at 25 options, so this view pages through the
    full channel list using Prev / Next buttons. The select is rebuilt in place
    on each page navigation.
    """

    PAGE_SIZE = 25  # Discord hard limit for select options

    def __init__(self, author_id: int, target_attr: str, attr_name: str, slot_label: str,
                 channels: List[Any], guild_name: str, original_view: "ChannelSetupView"):
        super().__init__(timeout=300)
        self.author_id = author_id
        self.target_attr = target_attr
        self.attr_name = attr_name
        self.slot_label = slot_label
        self.guild_name = guild_name
        self.original_view = original_view
        self.all_channels = channels
        self.total_pages = max(1, (len(channels) + self.PAGE_SIZE - 1) // self.PAGE_SIZE)
        self.page = 0

        self.channel_select = discord.ui.Select(
            placeholder=f"Choose a channel for: {slot_label}"[:150],
            min_values=1,
            max_values=1,
            options=self._page_options(),
        )
        self.channel_select.callback = self._on_channel_selected
        self.add_item(self.channel_select)

        self.prev_btn = Button(label="⬅ Prev", style=discord.ButtonStyle.secondary)
        self.prev_btn.callback = self._on_prev
        self.add_item(self.prev_btn)

        self.next_btn = Button(label="Next ➡", style=discord.ButtonStyle.secondary)
        self.next_btn.callback = self._on_next
        self.add_item(self.next_btn)

        self.back_btn = Button(label="⬅ Back to Slots", style=discord.ButtonStyle.secondary)
        self.back_btn.callback = self._on_back
        self.add_item(self.back_btn)

        self.clear_btn = Button(label="Clear", style=discord.ButtonStyle.danger)
        self.clear_btn.callback = self._on_clear
        self.add_item(self.clear_btn)

        self._sync_nav_state()

    # --- pagination helpers ---
    def _page_options(self) -> List[discord.SelectOption]:
        start = self.page * self.PAGE_SIZE
        page_channels = self.all_channels[start:start + self.PAGE_SIZE]
        return [
            discord.SelectOption(
                label=f"#{ch.name}"[:100],
                value=str(ch.id),
                description=f"ID: {ch.id}"[:100],
            )
            for ch in page_channels
        ]

    def _sync_nav_state(self) -> None:
        multi = self.total_pages > 1
        self.prev_btn.disabled = (not multi) or self.page <= 0
        self.next_btn.disabled = (not multi) or self.page >= self.total_pages - 1

    def render_embed(self) -> discord.Embed:
        current_id = getattr(_csetup_target(self.target_attr), self.attr_name, 0)
        current_str = f"<#{current_id}> (`{current_id}`)" if current_id else "`Not set`"
        embed = discord.Embed(
            title=f"🔧 Configure: {self.slot_label}",
            description=(
                f"Choose the Discord channel to assign to **{self.slot_label}**.\n"
                f"**Guild:** {self.guild_name}\n"
                f"**Current value:** {current_str}\n"
                f"**Available channels:** {len(self.all_channels)}"
            ),
            color=discord.Color.blurple(),
            timestamp=datetime.now(timezone.utc),
        )
        if self.total_pages > 1:
            start = self.page * self.PAGE_SIZE
            end = min(start + self.PAGE_SIZE, len(self.all_channels))
            embed.set_footer(
                text=f"Page {self.page + 1}/{self.total_pages} • Channels {start + 1}–{end} of {len(self.all_channels)} (A–Z) • Use Prev/Next"
            )
        return embed

    async def _on_prev(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("This setup menu is not for you.", ephemeral=True)
            return
        if self.page <= 0:
            await interaction.response.defer()
            return
        self.page -= 1
        self.channel_select.options = self._page_options()
        self._sync_nav_state()
        await interaction.response.edit_message(embed=self.render_embed(), view=self)

    async def _on_next(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("This setup menu is not for you.", ephemeral=True)
            return
        if self.page >= self.total_pages - 1:
            await interaction.response.defer()
            return
        self.page += 1
        self.channel_select.options = self._page_options()
        self._sync_nav_state()
        await interaction.response.edit_message(embed=self.render_embed(), view=self)

    async def _on_back(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("This setup menu is not for you.", ephemeral=True)
            return
        await interaction.response.edit_message(embed=_csetup_main_embed(), view=self.original_view)

    async def _on_clear(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("This setup menu is not for you.", ephemeral=True)
            return
        setattr(_csetup_target(self.target_attr), self.attr_name, 0)
        config.save_channel_settings()
        await interaction.response.edit_message(embed=_csetup_main_embed(), view=self.original_view)
        await interaction.followup.send(
            embed=EmbedBuilder.success("Channel Cleared", f"**{self.slot_label}** has been cleared."),
            ephemeral=True,
        )

    async def _on_channel_selected(self, interaction: discord.Interaction) -> None:
        if interaction.user.id != self.author_id:
            await interaction.response.send_message("This setup menu is not for you.", ephemeral=True)
            return
        channel_id = int(self.channel_select.values[0])
        setattr(_csetup_target(self.target_attr), self.attr_name, channel_id)
        config.save_channel_settings()
        await interaction.response.edit_message(embed=_csetup_main_embed(), view=self.original_view)
        await interaction.followup.send(
            embed=EmbedBuilder.success("Channel Updated", f"**{self.slot_label}** set to <#{channel_id}> (`{channel_id}`)."),
            ephemeral=True,
        )


@bot.hybrid_command(name="csetup", aliases=["CSetup", "CSETUP", "channelsetup", "ChannelSetup"])
@commands.has_permissions(manage_channels=True)
async def csetup_cmd(ctx: commands.Context) -> None:
    """Interactive channel setup via dropdown menus (!CSetup)."""
    view = ChannelSetupView(ctx)
    view.message = await ctx.send(embed=_csetup_main_embed(), view=view)


# --- MODERATION COMMANDS ---
@bot.hybrid_command()
@commands.has_permissions(kick_members=True)
async def kick(ctx: commands.Context, members: commands.Greedy[discord.Member], *, reason: str = "No reason provided") -> None:
    """Kicks one or multiple members. Usage: !kick @user1 @user2 [reason]"""
    if not members:
        await ctx.send("Usage: `!kick @user1 @user2 [reason]` (or `!kick @user1, @user2`)")
        return

    kicked_list = []
    failed_list = []

    for member in members:
        if member.top_role >= ctx.guild.me.top_role or member.id == ctx.guild.owner_id:
            failed_list.append(f"{member.mention} (Hierarchy/Owner)")
            continue
        try:
            await member.kick(reason=f"Kicked by {ctx.author}: {reason}")
            kicked_list.append(member.mention)
        except discord.Forbidden:
            failed_list.append(f"{member.mention} (Missing Perms)")
        except discord.HTTPException:
            failed_list.append(f"{member.mention} (API Error)")

    embed = EmbedBuilder.success("Mass Kick Complete", "")
    if kicked_list:
        embed.add_field(name="✅ Successfully Kicked", value="\n".join(kicked_list), inline=False)
    if failed_list:
        embed.add_field(name="❌ Failed to Kick", value="\n".join(failed_list), inline=False)
    
    embed.description = f"**Reason:** {reason}"
    await ctx.send(embed=embed)
    logging.info(f'User(s) {kicked_list} were kicked by {ctx.author} for: {reason}')


@bot.hybrid_command()
@commands.has_permissions(ban_members=True)
async def ban(ctx: commands.Context, members: commands.Greedy[discord.Member], *, reason: str = "No reason provided") -> None:
    """Bans one or multiple members. Usage: !ban @user1 @user2 [reason]"""
    if not members:
        await ctx.send("Usage: `!ban @user1 @user2 [reason]` (or `!ban @user1, @user2`)")
        return

    banned_list = []
    failed_list = []

    for member in members:
        if member.top_role >= ctx.guild.me.top_role or member.id == ctx.guild.owner_id:
            failed_list.append(f"{member.mention} (Hierarchy/Owner)")
            continue
        try:
            await member.ban(reason=f"Banned by {ctx.author}: {reason}", delete_message_days=0)
            banned_list.append(member.mention)
        except discord.Forbidden:
            failed_list.append(f"{member.mention} (Missing Perms)")
        except discord.HTTPException:
            failed_list.append(f"{member.mention} (API Error)")

    embed = EmbedBuilder.success("Mass Ban Complete", "")
    if banned_list:
        embed.add_field(name="✅ Successfully Banned", value="\n".join(banned_list), inline=False)
    if failed_list:
        embed.add_field(name="❌ Failed to Ban", value="\n".join(failed_list), inline=False)
    
    embed.description = f"**Reason:** {reason}"
    await ctx.send(embed=embed)
    logging.info(f'User(s) {banned_list} were banned by {ctx.author} for: {reason}')

@bot.hybrid_command()
@commands.has_permissions(ban_members=True)
async def banid(ctx: commands.Context, user_id: int, *, reason: str = "No reason provided") -> None:
    try:
        user = await bot.fetch_user(user_id)
        await ctx.guild.ban(user, reason=reason)
        await ctx.send(embed=EmbedBuilder.success("User Banned", f"**{user.name}** (ID: {user_id}) has been banned.\n**Reason:** {reason}"))
        logging.info(f'User with ID {user_id} was banned by {ctx.author} for: {reason}')
    except discord.NotFound:
        await ctx.send(f'User with ID {user_id} not found.')
    except discord.Forbidden:
        await ctx.send("I don't have permission to ban that user.")
    except discord.HTTPException:
        await ctx.send("Failed to ban the user. Please try again.")


# --- BLACKLIST COMMANDS ---
@bot.hybrid_command(name="blacklist", description="Add a keyword to the blacklist")
@commands.has_permissions(administrator=True)
@app_commands.describe(keyword="The keyword to blacklist")
async def blacklist_cmd(ctx: commands.Context, *, keyword: str) -> None:
    global blacklisted_keywords
    
    keyword = keyword.strip()
    if not keyword:
        await ctx.send("Please provide a keyword to blacklist.")
        return
    
    if len(keyword) < config.limits.min_blacklist_keyword_length:
        await ctx.send(f"Keyword must be at least {config.limits.min_blacklist_keyword_length} characters long.")
        return
    
    for existing in blacklisted_keywords:
        if existing.lower() == keyword.lower():
            await ctx.send(f"Keyword `{keyword}` is already blacklisted.")
            return
    
    blacklisted_keywords.add(keyword)
    save_blacklist_data()
    
    embed = EmbedBuilder.warning(
        "Keyword Blacklisted",
        f"Added `{keyword}` to blacklist.\nTotal keywords: {len(blacklisted_keywords)}\n\n"
        f"Any user with this keyword in their profile will be auto-banned."
    )
    
    await ctx.send(embed=embed)
    logging.info(f"[Blacklist] Keyword '{keyword}' added by {ctx.author}")


@bot.hybrid_command(name="unblacklist", description="Remove a keyword from the blacklist")
@commands.has_permissions(administrator=True)
@app_commands.describe(keyword="The keyword to remove from the blacklist")
@app_commands.autocomplete(keyword=blacklist_autocomplete)
async def unblacklist_cmd(ctx: commands.Context, *, keyword: str) -> None:
    global blacklisted_keywords
    
    keyword = keyword.strip()
    found_keyword = next((k for k in blacklisted_keywords if k.lower() == keyword.lower()), None)
    
    if not found_keyword:
        await ctx.send(f"Keyword `{keyword}` is not in the blacklist.")
        return
    
    blacklisted_keywords.discard(found_keyword)
    save_blacklist_data()
    
    await ctx.send(embed=EmbedBuilder.success("Keyword Removed", f"Removed `{found_keyword}` from blacklist."))
    logging.info(f"[Blacklist] Keyword '{found_keyword}' removed by {ctx.author}")


@bot.hybrid_command(name="blacklistscan", description="Scan all members for blacklisted keywords")
@commands.has_permissions(administrator=True)
async def blacklistscan_cmd(ctx: commands.Context) -> None:
    if not blacklisted_keywords:
        await ctx.send("No keywords are currently blacklisted. Use `/blacklist <keyword>` to add some.")
        return
    
    status_msg = await ctx.send(f"Scanning **{ctx.guild.member_count}** members for **{len(blacklisted_keywords)}** blacklisted keyword(s)...")
    
    banned_count, failed_count, matches = await scan_and_ban_blacklisted_members(ctx.guild)
    await status_msg.delete()
    
    embed = EmbedBuilder.warning(
        "Blacklist Scan Complete",
        f"**Members Scanned:** {ctx.guild.member_count}\n"
        f"**Members Banned:** {banned_count}\n"
        f"**Failed to Ban:** {failed_count}"
    )
    
    if matches:
        match_text = ""
        for match in matches[:5]:
            match_text += f"- {match['user'].name} - \"{match['keyword']}\" in {match['location']}\n"
        if len(matches) > 5:
            match_text += f"... and {len(matches) - 5} more"
        embed.add_field(name="Matches Found", value=match_text, inline=False)
    
    await ctx.send(embed=embed)
    logging.info(f"[Blacklist] Scan complete by {ctx.author}. Banned: {banned_count}, Failed: {failed_count}")


@bot.hybrid_command(name="blacklistlist", description="Display all blacklisted keywords")
@commands.has_permissions(administrator=True)
async def blacklistlist_cmd(ctx: commands.Context) -> None:
    if not blacklisted_keywords:
        await ctx.send("**No keywords are currently blacklisted.**")
        return
    
    keywords_list = list(blacklisted_keywords)
    embed = discord.Embed(title="Blacklisted Keywords", color=discord.Color.red())
    
    chunks = [keywords_list[i:i+20] for i in range(0, len(keywords_list), 20)]
    for i, chunk in enumerate(chunks[:5]):
        field_name = "Keywords" if i == 0 else "Keywords (continued)"
        embed.add_field(name=field_name, value="\n".join(f"- `{kw}`" for kw in chunk), inline=False)
    
    if len(chunks) > 5:
        embed.add_field(name="...", value=f"And {len(keywords_list) - 100} more keywords", inline=False)
    
    embed.set_footer(text=f"Total: {len(blacklisted_keywords)} keyword(s)")
    await ctx.send(embed=embed)


@bot.hybrid_command(name="checkprofile", description="Check a user's profile for blacklisted keywords")
@app_commands.describe(member="The member to check")
async def checkprofile_cmd(ctx: commands.Context, member: Optional[discord.Member] = None) -> None:
    if member is None:
        member = ctx.author
    
    is_blacklisted, keyword, location = await check_user_profile_for_blacklist(member)
    
    embed = discord.Embed(title=f"Profile Check: {member.display_name}", color=discord.Color.red() if is_blacklisted else discord.Color.green())
    embed.set_thumbnail(url=member.avatar.url if member.avatar else member.default_avatar.url)
    
    if is_blacklisted:
        embed.add_field(name="Status", value="**BLACKLISTED**", inline=True)
        embed.add_field(name="Matched Keyword", value=f"**{keyword}**", inline=True)
        embed.add_field(name="Location", value=location, inline=True)
    else:
        embed.add_field(name="Status", value="**Clean**", inline=True)
        embed.add_field(name="Keywords Checked", value=str(len(blacklisted_keywords)), inline=True)
    
    await ctx.send(embed=embed)


# --- RULES MANAGEMENT COMMANDS ---
@bot.hybrid_command(name="updategangrules", description="Manually update the cached GNG rules")
@commands.has_permissions(administrator=True)
async def updategangrules_cmd(ctx: commands.Context) -> None:
    try:
        channel = bot.get_channel(config.servers.gang_rules_channel)
        if not channel:
            await ctx.send("GNG rules channel not found.")
            return
        message = await channel.fetch_message(config.servers.gang_rules_message)
        rules_cache['gang_rules'] = message.content
        rules_cache['gang_last_updated'] = datetime.now(timezone.utc).isoformat()
        save_rules_cache()
        
        embed = EmbedBuilder.success("GNG Rules Updated", f"Successfully cached the GNG rules.\n**Characters:** {len(message.content)}")
        await ctx.send(embed=embed)
        logging.info(f"[RulesCache] GNG rules manually updated by {ctx.author}")
    except discord.NotFound:
        await ctx.send("GNG rules message not found.")
    except discord.Forbidden:
        await ctx.send("No permission to fetch the GNG rules message.")
    except Exception as e:
        await ctx.send(f"Error: {str(e)}")


@bot.hybrid_command(name="updateserverrules", description="Manually update the cached Server rules")
@commands.has_permissions(administrator=True)
async def updateserverrules_cmd(ctx: commands.Context) -> None:
    try:
        server_guild = bot.get_guild(config.servers.server_server_id)
        if not server_guild:
            await ctx.send("Bot is not in the Server server. Cannot fetch rules.\nUse `/setserverrules` to manually set the rules.")
            return
        channel = server_guild.get_channel(config.servers.server_rules_channel)
        if not channel:
            await ctx.send("Server rules channel not found.")
            return
        message = await channel.fetch_message(config.servers.server_rules_message)
        rules_cache['server_rules'] = message.content
        rules_cache['server_last_updated'] = datetime.now(timezone.utc).isoformat()
        save_rules_cache()
        
        embed = EmbedBuilder.success("Server Rules Updated", f"Successfully cached the Server rules.\n**Characters:** {len(message.content)}")
        await ctx.send(embed=embed)
        logging.info(f"[RulesCache] Server rules manually updated by {ctx.author}")
    except discord.NotFound:
        await ctx.send("Server rules message not found.")
    except discord.Forbidden:
        await ctx.send("No permission to fetch the Server rules message.")
    except Exception as e:
        await ctx.send(f"Error: {str(e)}")


@bot.hybrid_command(name="setserverrules", description="Manually set the Server rules text")
@commands.has_permissions(administrator=True)
async def setserverrules_cmd(ctx: commands.Context, *, rules: str) -> None:
    rules_cache['server_rules'] = rules
    rules_cache['server_last_updated'] = datetime.now(timezone.utc).isoformat()
    save_rules_cache()
    
    embed = EmbedBuilder.success("Server Rules Set", f"Successfully saved the Server rules.\n**Characters:** {len(rules)}")
    await ctx.send(embed=embed)
    logging.info(f"[RulesCache] Server rules manually set by {ctx.author}")


@bot.hybrid_command(name="setgangrules", description="Manually set the GNG rules text")
@commands.has_permissions(administrator=True)
async def setgangrules_cmd(ctx: commands.Context, *, rules: str) -> None:
    rules_cache['gang_rules'] = rules
    rules_cache['gang_last_updated'] = datetime.now(timezone.utc).isoformat()
    save_rules_cache()
    
    embed = EmbedBuilder.success("GNG Rules Set", f"Successfully saved the GNG rules.\n**Characters:** {len(rules)}")
    await ctx.send(embed=embed)
    logging.info(f"[RulesCache] GNG rules manually set by {ctx.author}")


@bot.hybrid_command(name="viewcachedrules", description="View the currently cached rules")
@commands.has_permissions(administrator=True)
async def viewcachedrules_cmd(ctx: commands.Context) -> None:
    embed = discord.Embed(title="Cached Rules Status", color=discord.Color.blurple())
    
    gang_status = f"Cached ({len(rules_cache['gang_rules'])} chars)" if rules_cache['gang_rules'] else "Not cached"
    server_status = f"Cached ({len(rules_cache['server_rules'])} chars)" if rules_cache['server_rules'] else "Not cached"
    
    embed.add_field(name="GNG Rules", value=f"{gang_status}\nLast Updated: {rules_cache['gang_last_updated'][:19] if rules_cache['gang_last_updated'] else 'Never'}", inline=True)
    embed.add_field(name="Server Rules", value=f"{server_status}\nLast Updated: {rules_cache['server_last_updated'][:19] if rules_cache['server_last_updated'] else 'Never'}", inline=True)
    
    await ctx.send(embed=embed)


# --- PURGE / CHANNEL COMMANDS ---
@bot.hybrid_command(name="purge", aliases=["purgeall"], description="Purge a specific amount of messages or the entire channel")
@commands.has_permissions(manage_messages=True)
@app_commands.describe(
    amount="Number of messages to delete. Leave empty to delete the whole channel.",
    flags="Optional flags. Use '-del' to also delete the !purge command message and skip the confirmation reply.",
)
async def purge_cmd(
    ctx: commands.Context,
    amount: Optional[int] = None,
    *,
    flags: str = "",
) -> None:
    """Purges a specific amount of messages, or the entire channel if no amount is given.

    NEW FLAGS / FIXES:
      - `-del` flag (prefix `!purge 2 -del` or slash `/purge amount:2 flags:"-del"`):
          * Also deletes the `!purge` command message itself after purging.
          * Skips the "Deleted N messages." confirmation reply entirely.
          * The cleanup-command prompt (for chained commands) is also skipped,
            since the command message is gone.
      - Message-log sync:
          * discord.py's `TextChannel.purge()` uses the bulk-delete endpoint,
            which does NOT fire `on_message_delete` for individual messages.
            So previously, purged messages never appeared in the msg log.
          * We now fetch the target messages first, log them via
            `MessageLogSystem.log_bulk_delete`, THEN bulk-delete. The log
            channel gets a single compact summary embed (not one per msg).
      - `before=ctx.message`:
          * Keeps the command message out of the fetch set entirely (the old
            `check=` approach wasted a `limit` slot on the command message).
    """
    # `flags` is a keyword-only string that absorbs any trailing tokens after
    # `amount` for prefix invocations (e.g. `!purge 2 -del` -> amount=2,
    # flags="-del"). For slash, the user passes flags="-del" explicitly.
    delete_command = False
    # Check both the raw message content (most reliable for prefix) and the
    # parsed `flags` kwarg (covers slash and edge cases).
    if ctx.message is not None and ctx.message.content:
        content_lower = ctx.message.content.lower()
        if ' -del' in content_lower or content_lower.endswith('-del'):
            delete_command = True
    if flags and '-del' in flags.lower():
        delete_command = True

    before_obj = ctx.message
    cmd_id = ctx.message.id if ctx.message else None

    def _skip_command(m: discord.Message) -> bool:
        return cmd_id is None or m.id != cmd_id

    # Helper that fetches a batch of messages, logs them to the msg log,
    # then bulk-deletes them. Returns the list of deleted messages.
    # We do the fetch+log+delete ourselves (instead of ctx.channel.purge)
    # so we can log each message before it's gone — purge()'s bulk delete
    # never fires on_message_delete, which is why purged msgs were missing
    # from the msg log.
    async def _fetch_log_delete(channel: discord.TextChannel, limit: int, before) -> List[discord.Message]:
        batch: List[discord.Message] = []
        async for m in channel.history(limit=limit, before=before, oldest_first=False):
            batch.append(m)
        if not batch:
            return []
        # Log to the msg-log channel BEFORE deleting.
        try:
            await MessageLogSystem.log_bulk_delete(
                batch, ctx.author, channel,
                reason=f"purge {limit}" if amount else "purge all",
            )
        except Exception as e:
            logging.warning(f"[Purge] msglog sync failed (non-fatal): {e}")
        # Bulk-delete. delete_messages accepts up to 100 messages at once.
        try:
            await channel.delete_messages(batch)
        except discord.HTTPException:
            # Fallback: delete one at a time (some are too old for bulk).
            for m in batch:
                try:
                    await m.delete()
                except (discord.HTTPException, discord.NotFound):
                    pass
        return batch

    if amount is None:
        # --- PURGE ALL LOGIC ---
        deleted_messages: List[discord.Message] = []
        while True:
            try:
                batch = await _fetch_log_delete(ctx.channel, 100, before_obj)
                deleted_messages.extend(batch)
                if len(batch) < 100:
                    break
                await asyncio.sleep(1)
            except discord.HTTPException as e:
                if e.status == 429:
                    retry_after = int(e.response.headers.get('Retry-After', 1)) if hasattr(e.response, 'headers') else 1
                    await asyncio.sleep(retry_after)
                else:
                    break

        deleted_count = len(deleted_messages)
        logging.info(f'{ctx.author} purged all ({deleted_count}) messages in {ctx.channel}')

        if not delete_command:
            message = await ctx.send(f'Deleted {deleted_count} messages.')
            await asyncio.sleep(2)
            try:
                await message.delete()
            except (discord.HTTPException, discord.Forbidden, discord.NotFound):
                pass
        else:
            # -del: delete the command message itself. The cleanup prompt for
            # chained commands is also skipped (see process_potential_multi_command).
            if cmd_id is not None:
                try:
                    await ctx.message.delete()
                except (discord.HTTPException, discord.Forbidden, discord.NotFound):
                    pass

    else:
        # --- PURGE SPECIFIC AMOUNT LOGIC ---
        if amount <= 0:
            await ctx.send("Amount must be a positive number.", ephemeral=True)
            return

        if before_obj is not None:
            deleted_messages = await _fetch_log_delete(ctx.channel, amount, before_obj)
        else:
            # Slash fallback: fetch one extra to compensate for the skipped
            # command message, then filter it out before logging/deleting.
            batch: List[discord.Message] = []
            async for m in ctx.channel.history(limit=amount + 1, oldest_first=False):
                if _skip_command(m):
                    batch.append(m)
            if batch:
                try:
                    await MessageLogSystem.log_bulk_delete(
                        batch, ctx.author, ctx.channel,
                        reason=f"purge {amount}",
                    )
                except Exception as e:
                    logging.warning(f"[Purge] msglog sync failed (non-fatal): {e}")
                try:
                    await ctx.channel.delete_messages(batch)
                except discord.HTTPException:
                    for m in batch:
                        try:
                            await m.delete()
                        except (discord.HTTPException, discord.NotFound):
                            pass
            deleted_messages = batch

        deleted_count = len(deleted_messages)
        logging.info(f'{ctx.author} purged {deleted_count} messages in {ctx.channel}')

        if not delete_command:
            message = await ctx.send(f'Deleted {deleted_count} messages.')
            await asyncio.sleep(2)
            try:
                await message.delete()
            except (discord.HTTPException, discord.Forbidden, discord.NotFound):
                pass
        else:
            # -del: also delete the original command message.
            if cmd_id is not None:
                try:
                    await ctx.message.delete()
                except (discord.HTTPException, discord.Forbidden, discord.NotFound):
                    pass


@bot.hybrid_command()
@commands.has_permissions(manage_roles=True)
async def mute(ctx: commands.Context, member: discord.Member, *, reason: Optional[str] = None) -> None:
    if config.roles.staff in [role.id for role in member.roles]:
        await ctx.send(f'{member.mention} cannot be muted because they have the Staff role.')
        return
    
    mute_role = discord.utils.get(ctx.guild.roles, name='Muted')
    if not mute_role:
        message = await ctx.send('Mute role not found. Please create a role named "Muted".')
        await asyncio.sleep(2)
        await message.delete()
        return
    
    try:
        await member.add_roles(mute_role)
        response = f'Muted {member.mention} for: {reason}' if reason else f'Muted {member.mention} without a specified reason.'
        await ctx.send(embed=EmbedBuilder.warning("Member Muted", response))
        logging.info(f'User {member} was muted by {ctx.author} for: {reason}')
    except discord.Forbidden:
        await ctx.send("I do not have permission to mute that member.")
    except discord.HTTPException:
        await ctx.send("Failed to mute the member. Please try again.")


# NOTE: The `unmute` command is defined further below alongside the temp-mute
# system. It removes the Muted role AND deactivates any active temp-mute
# records in the database (so a manual unmute cancels a pending auto-unmute).


@bot.hybrid_command()
@commands.has_permissions(manage_channels=True)
async def lock(ctx: commands.Context) -> None:
    await ctx.channel.set_permissions(ctx.guild.default_role, send_messages=False)
    await ctx.send(embed=EmbedBuilder.warning("Channel Locked", "This channel is now locked."))
    logging.info(f'Channel {ctx.channel} was locked by {ctx.author}')


@bot.hybrid_command()
@commands.has_permissions(manage_channels=True)
async def unlock(ctx: commands.Context) -> None:
    await ctx.channel.set_permissions(ctx.guild.default_role, send_messages=True)
    await ctx.send(embed=EmbedBuilder.success("Channel Unlocked", "This channel is now unlocked."))
    logging.info(f'Channel {ctx.channel} was unlocked by {ctx.author}')


@bot.hybrid_command()
@commands.has_permissions(manage_channels=True)
async def slowmode(ctx: commands.Context, seconds: int) -> None:
    await ctx.channel.edit(slowmode_delay=seconds)
    await ctx.send(embed=EmbedBuilder.info("Slowmode Set", f"Slowmode set to {seconds} seconds."))
    logging.info(f'Slowmode in {ctx.channel} was set to {seconds}s by {ctx.author}')


@bot.hybrid_command()
@commands.has_permissions(manage_roles=True)
async def addrole(ctx: commands.Context, member: discord.Member, *, role_name: str) -> None:
    role = discord.utils.get(ctx.guild.roles, name=role_name)
    if role is None:
        await ctx.send(f'Role "{role_name}" not found.')
        return
    if role in member.roles:
        await ctx.send(f"{member.mention} already has the {role_name} role.")
        return
    await member.add_roles(role)
    await ctx.send(embed=EmbedBuilder.success("Role Added", f"Added **{role_name}** to {member.mention}."))
    log_event("Role Added", ctx.author, f"Added {role_name} to {member}")


@bot.hybrid_command()
@commands.has_permissions(manage_roles=True)
async def roleall(ctx: commands.Context, role: discord.Role) -> None:
    if role is None:
        await ctx.send("Please mention a valid role.")
        return
    
    members_assigned = 0
    failed_members = 0
    
    for member in ctx.guild.members:
        if member.bot:
            continue
        if role not in member.roles:
            try:
                await member.add_roles(role)
                members_assigned += 1
            except (discord.Forbidden, discord.HTTPException):
                failed_members += 1
    
    await ctx.send(embed=EmbedBuilder.success("Role Assigned", f"Assigned {role.mention} to **{members_assigned}** members.{f' Failed: {failed_members}' if failed_members else ''}"))
    log_event("Role All Assigned", ctx.author, f"Assigned {role.name} to {members_assigned} members")


@bot.hybrid_command()
@commands.has_permissions(manage_roles=True)
async def removerole(ctx: commands.Context, member: discord.Member, role: discord.Role) -> None:
    try:
        await member.remove_roles(role)
        await ctx.send(embed=EmbedBuilder.success("Role Removed", f"Removed **{role.name}** from {member.mention}."))
        logging.info(f'Role {role.name} was removed from {member} by {ctx.author}')
    except discord.Forbidden:
        await ctx.send("I do not have permission to remove that role.")
    except discord.HTTPException:
        await ctx.send("Failed to remove the role. Please try again.")


@bot.hybrid_command()
@commands.has_permissions(ban_members=True)
async def softban(ctx: commands.Context, member: discord.Member, *, reason: Optional[str] = None) -> None:
    await member.ban(reason=reason)
    await ctx.guild.unban(member)
    await ctx.send(embed=EmbedBuilder.warning("Member Softbanned", f"{member.mention} has been softbanned.\n**Reason:** {reason or 'No reason provided'}"))
    logging.info(f'User {member} was softbanned by {ctx.author} for: {reason}')


@bot.hybrid_command()
@commands.has_permissions(manage_roles=True)
async def tempmute(ctx: commands.Context, member: discord.Member, duration: int, *, reason: Optional[str] = None) -> None:
    """
    Temporarily mute a member for `duration` seconds.

    The mute is persisted in the database (temp_mutes table) and a background
    task performs the unmute, so the mute survives a bot restart instead of
    relying on a blocking asyncio.sleep() that dies when the process dies.
    """
    if duration <= 0:
        await ctx.send("Duration must be a positive number of seconds.")
        return

    mute_role = discord.utils.get(ctx.guild.roles, name='Muted')
    if not mute_role:
        await ctx.send("Mute role not found. Please create a role named 'Muted'.")
        return

    now = datetime.now(timezone.utc)
    unmute_at = now + timedelta(seconds=duration)
    mute_id = str(_uuid.uuid4())[:8]
    mute_record = {
        'mute_id': mute_id,
        'guild_id': ctx.guild.id,
        'user_id': member.id,
        'role_id': mute_role.id,
        'moderator_id': ctx.author.id,
        'reason': reason,
        'muted_at': now.isoformat(),
        'unmute_at': unmute_at.isoformat(),
        'is_active': True,
    }

    try:
        await member.add_roles(mute_role, reason=reason or "Temp-mute")
    except discord.Forbidden:
        await ctx.send("I don't have permission to manage roles for that member.")
        return
    except discord.HTTPException:
        await ctx.send("Failed to apply the mute role. Please try again.")
        return

    # Persist AFTER the role is applied so we only track mutes that actually took effect.
    data_manager.save_temp_mute(mute_record)
    logging.info(f'[TempMute] {member} (ID: {member.id}) muted by {ctx.author} for {duration}s (mute_id={mute_id})')

    await ctx.send(embed=EmbedBuilder.warning(
        "Member Temp-Muted",
        f"{member.mention} muted for **{duration}** seconds.\n"
        f"**Reason:** {reason or 'No reason provided'}\n"
        f"**Unmute at:** {unmute_at.strftime('%Y-%m-%d %H:%M:%S UTC')}\n"
        f"**Mute ID:** `{mute_id}`"
    ))

    # Schedule a prompt unmute in-memory. The DB-backed background task
    # (check_temp_mutes_task) is the persistent safety net that also handles
    # mutes that were in progress when the bot restarted.
    asyncio.create_task(_schedule_unmute(mute_id, duration))


async def _schedule_unmute(mute_id: str, delay: int) -> None:
    """Wait `delay` seconds then unmute. Dies gracefully if the bot restarts
    (the persistent check_temp_mutes_task will pick it up)."""
    try:
        await asyncio.sleep(delay)
    except asyncio.CancelledError:
        return
    await _unmute_user(mute_id, source="scheduled")


async def _unmute_user(mute_id: str, source: str = "scheduled") -> None:
    """Remove the mute role for a given mute_id and deactivate the record.
    Idempotent: safe to call multiple times (e.g. once by the scheduled task
    and again by the background loop)."""
    mute = data_manager.get_temp_mute(mute_id)
    if not mute or not mute.get('is_active'):
        return

    # Deactivate first so a concurrent call is a no-op.
    data_manager.deactivate_temp_mute(mute_id)

    guild = bot.get_guild(mute['guild_id'])
    if not guild:
        logging.warning(f"[TempMute] Guild {mute['guild_id']} not found for unmute (mute_id={mute_id})")
        return

    role = guild.get_role(mute['role_id'])
    member = guild.get_member(mute['user_id'])

    if member and role:
        try:
            await member.remove_roles(role, reason=f"Temp-mute expired (mute_id={mute_id})")
        except discord.Forbidden:
            logging.warning(f"[TempMute] No permission to remove mute role from {member}")
        except discord.HTTPException as e:
            logging.error(f"[TempMute] Failed to remove mute role from {member}: {e}")
    elif member is None:
        # Member left the guild; the role can't be removed now, but the record
        # is deactivated so they won't be re-muted on rejoin handling. If they
        # rejoin, they simply won't have the role.
        logging.info(f"[TempMute] Member {mute['user_id']} not in guild; role removal skipped (mute_id={mute_id})")

    logging.info(f"[TempMute] Unmuted user {mute['user_id']} via {source} (mute_id={mute_id})")

    log_channel = bot.get_channel(config.channels.log)
    if log_channel:
        try:
            embed = discord.Embed(
                title="🔇 Temp-Mute Expired",
                color=discord.Color.green(),
                timestamp=datetime.now(timezone.utc),
            )
            embed.add_field(name="User", value=f"<@{mute['user_id']}> (`{mute['user_id']}`)", inline=True)
            embed.add_field(name="Mute ID", value=f"`{mute_id}`", inline=True)
            embed.add_field(name="Triggered By", value=source, inline=True)
            embed.add_field(name="Original Reason", value=mute.get('reason') or "No reason provided", inline=False)
            await log_channel.send(embed=embed)
        except Exception as e:
            logging.warning(f"[TempMute] Could not send unmute log: {e}")


async def restore_temp_mutes() -> None:
    """Called on startup. Re-schedules prompt unmutes for any active mutes
    and immediately unmutes any that already expired while the bot was down."""
    active = data_manager.load_active_temp_mutes()
    if not active:
        return
    now = datetime.now(timezone.utc)
    scheduled = 0
    expired_now = 0
    for mute in active:
        try:
            unmute_at = datetime.fromisoformat(mute['unmute_at'].replace('Z', '+00:00'))
        except Exception:
            logging.warning(f"[TempMute] Could not parse unmute_at for mute_id={mute.get('mute_id')}; deactivating.")
            data_manager.deactivate_temp_mute(mute.get('mute_id'))
            continue

        remaining = (unmute_at - now).total_seconds()
        if remaining <= 0:
            await _unmute_user(mute['mute_id'], source="startup-expired")
            expired_now += 1
        else:
            asyncio.create_task(_schedule_unmute(mute['mute_id'], int(remaining) + 1))
            scheduled += 1

    logging.info(f"[TempMute] Restored {scheduled} active mute(s); unmuted {expired_now} expired mute(s) on startup.")


@tasks.loop(minutes=1)
async def check_temp_mutes_task() -> None:
    """Persistent safety net: unmute any active temp-mute whose time is up.
    Catches mutes whose in-memory scheduled task was lost (e.g. after a restart)
    or that were created before the scheduling helper existed."""
    try:
        now_iso = datetime.now(timezone.utc).isoformat()
        expired = data_manager.load_expired_temp_mutes(now_iso)
        for mute in expired:
            await _unmute_user(mute['mute_id'], source="background-loop")
    except Exception as e:
        logging.error(f"[TempMute] Error in check_temp_mutes_task: {e}")


@check_temp_mutes_task.before_loop
async def before_check_temp_mutes() -> None:
    await bot.wait_until_ready()


# --- Message Log cache pruning (keeps the SQLite cache bounded) ---
@tasks.loop(minutes=30)
async def prune_message_cache_task() -> None:
    """Periodically trim the message-log cache so it never grows unbounded."""
    try:
        if data_manager is None or data_manager._connection is None:
            return
        removed = data_manager.prune_message_cache(keep_recent=5000)
        if removed:
            logging.info(f"[MsgLog] Pruned {removed} stale cached message(s)")
    except Exception as exc:
        logging.debug(f"[MsgLog] prune task error: {exc}")


@prune_message_cache_task.before_loop
async def before_prune_message_cache() -> None:
    await bot.wait_until_ready()


@bot.hybrid_command()
@commands.has_permissions(manage_roles=True)
async def unmute(ctx: commands.Context, member: discord.Member) -> None:
    """Manually unmute a member early, deactivating any active temp-mute."""
    mute_role = discord.utils.get(ctx.guild.roles, name='Muted')
    if not mute_role:
        await ctx.send("Mute role not found.")
        return

    # Deactivate any active DB records for this user in this guild.
    removed = 0
    for mute in data_manager.load_active_temp_mutes():
        if mute.get('guild_id') == ctx.guild.id and mute.get('user_id') == member.id:
            data_manager.deactivate_temp_mute(mute['mute_id'])
            removed += 1

    try:
        if mute_role in member.roles:
            await member.remove_roles(mute_role, reason=f"Manually unmuted by {ctx.author}")
        await ctx.send(embed=EmbedBuilder.success(
            "Member Unmuted",
            f"{member.mention} has been unmuted.\nDeactivated {removed} active temp-mute record(s)."
        ))
        logging.info(f"[TempMute] {member} manually unmuted by {ctx.author}; {removed} record(s) deactivated.")
    except discord.Forbidden:
        await ctx.send("I don't have permission to manage roles for that member.")
    except discord.HTTPException:
        await ctx.send("Failed to remove the mute role. Please try again.")


# --- WARNING COMMANDS (V2 Enhancement) ---
@bot.hybrid_command(name="warn", description="Warn a member")
@commands.has_permissions(manage_roles=True)
@app_commands.describe(member="Member to warn", reason="Reason for warning")
async def warn_cmd(ctx: commands.Context, member: discord.Member, *, reason: str) -> None:
    if not config.enable_warnings:
        await ctx.send("Warning system is disabled.")
        return
    
    import uuid
    warning_id = str(uuid.uuid4())[:8]
    
    warning = {
        'warning_id': warning_id,
        'user_id': member.id,
        'guild_id': ctx.guild.id,
        'moderator_id': ctx.author.id,
        'reason': reason,
        'points': 1,
        'created_at': datetime.now(timezone.utc).isoformat(),
        'is_active': True
    }
    
    if ctx.guild.id not in warnings_data:
        warnings_data[ctx.guild.id] = []
    warnings_data[ctx.guild.id].append(warning)
    
    total_points = sum(1 for w in warnings_data[ctx.guild.id] if w['user_id'] == member.id and w['is_active'])
    
    embed = EmbedBuilder.warning(
        "Member Warned",
        f"{member.mention} has been warned.\n"
        f"**Reason:** {reason}\n"
        f"**Warning ID:** {warning_id}\n"
        f"**Total Points:** {total_points}/{config.limits.max_warnings_before_ban}"
    )
    
    await ctx.send(embed=embed)
    
    if total_points >= config.limits.max_warnings_before_ban and ows_get("warnings_auto_ban"):
        try:
            await member.ban(reason=f"Exceeded warning limit ({total_points} points)")
            await ctx.send(embed=EmbedBuilder.error("Auto-Ban", f"{member.mention} has been auto-banned for exceeding warning limit."))
        except:
            pass
    
    logging.info(f"[Warnings] {member} warned by {ctx.author}: {reason}")


@bot.hybrid_command(name="warnings", description="View warnings for a member")
@app_commands.describe(member="Member to check")
async def warnings_cmd(ctx: commands.Context, member: Optional[discord.Member] = None) -> None:
    member = member or ctx.author
    
    guild_warnings = warnings_data.get(ctx.guild.id, [])
    user_warnings = [w for w in guild_warnings if w['user_id'] == member.id and w['is_active']]
    
    if not user_warnings:
        await ctx.send(embed=EmbedBuilder.info("Warnings", f"{member.mention} has no active warnings."))
        return
    
    embed = EmbedBuilder.warning(f"Warnings for {member.display_name}", f"Total Active: {len(user_warnings)}")
    
    for w in user_warnings[:5]:
        created = datetime.fromisoformat(w['created_at']).strftime('%Y-%m-%d')
        embed.add_field(
            name=f"Warning {w['warning_id']}",
            value=f"**Reason:** {w['reason']}\n**By:** <@{w['moderator_id']}>\n**Date:** {created}",
            inline=False
        )
    
    await ctx.send(embed=embed)


@bot.hybrid_command(name="clearwarnings", description="Clear warnings for a member")
@commands.has_permissions(administrator=True)
@app_commands.describe(member="Member to clear warnings for")
async def clearwarnings_cmd(ctx: commands.Context, member: discord.Member) -> None:
    global warnings_data
    
    if ctx.guild.id not in warnings_data:
        await ctx.send(f"{member.mention} has no warnings.")
        return
    
    count = 0
    for w in warnings_data[ctx.guild.id]:
        if w['user_id'] == member.id and w['is_active']:
            w['is_active'] = False
            count += 1
    
    await ctx.send(embed=EmbedBuilder.success("Warnings Cleared", f"Cleared {count} warning(s) for {member.mention}."))
    logging.info(f"[Warnings] {ctx.author} cleared {count} warnings for {member}")


# =============================================================================
# --- TICKET TOOL COMMANDS (Full Ticket Tool Clone) ---
@bot.hybrid_command(name="panel", description="Create a new ticket panel")
@commands.has_permissions(manage_channels=True)
async def create_panel(ctx: commands.Context) -> None:
    """Open the interactive panel creator."""
    view = PanelCreatorView(ctx.guild.id, ctx.author.id)
    embed = discord.Embed(
        title="Panel Creator",
        description="Use the buttons below to configure your ticket panel.\n\n"
                    "**Steps:**\n"
                    "1. Set Name - Give your panel a name\n"
                    "2. Set Embed - Customize the embed appearance\n"
                    "3. Set Button - Customize the create button\n"
                    "4. Settings - Configure ticket limits and more\n"
                    "5. Preview - See how it will look\n"
                    "6. Create Panel - Send the panel to this channel",
        color=discord.Color.blurple()
    )
    await ctx.send(embed=embed, view=view)


@bot.hybrid_command(name="panels", description="List all ticket panels")
@commands.has_permissions(manage_channels=True)
async def list_panels(ctx: commands.Context) -> None:
    """List all ticket panels in this server."""
    panels = data_manager.load_ticket_panels_by_guild(ctx.guild.id)
    
    if not panels:
        await ctx.send("No ticket panels found. Use `/panel` to create one.")
        return
    
    embed = discord.Embed(
        title="Ticket Panels",
        description=f"Found **{len(panels)}** panel(s) in this server:",
        color=discord.Color.blurple()
    )
    
    for panel in panels[:10]:
        channel = ctx.guild.get_channel(panel.get('channel_id'))
        channel_name = channel.mention if channel else "Unknown"
        embed.add_field(
            name=f"{panel.get('name', 'Unnamed')} (ID: {panel['panel_id']})",
            value=f"Channel: {channel_name}\nButton: {panel.get('button_label', 'Create Ticket')}",
            inline=False
        )
    
    await ctx.send(embed=embed)


@bot.hybrid_command(name="deletepanel", description="Delete a ticket panel")
@commands.has_permissions(manage_channels=True)
@app_commands.describe(panel_id="The panel ID to delete")
async def delete_panel(ctx: commands.Context, panel_id: str) -> None:
    """Delete a ticket panel."""
    panel = data_manager.load_ticket_panel(panel_id)
    
    if not panel or panel['guild_id'] != ctx.guild.id:
        await ctx.send("Panel not found in this server.")
        return
    
    if panel.get('channel_id') and panel.get('message_id'):
        try:
            channel = ctx.guild.get_channel(panel['channel_id'])
            if channel:
                message = await channel.fetch_message(panel['message_id'])
                await message.delete()
        except:
            pass
    
    data_manager.delete_ticket_panel(panel_id)
    await ctx.send(f"Panel `{panel_id}` has been deleted.")


@bot.hybrid_command(name="claim", description="Claim the current ticket")
async def claim_ticket_cmd(ctx: commands.Context) -> None:
    """Claim a ticket."""
    if not ticket_tool:
        await ctx.send("Ticket system not initialized.")
        return
    
    success, message = await ticket_tool.claim_ticket(ctx.channel, ctx.author)
    
    if success:
        await ctx.send(embed=discord.Embed(title="Ticket Claimed", description=message, color=discord.Color.green()))
    else:
        await ctx.send(message)


@bot.hybrid_command(name="unclaim", description="Release your claim on this ticket")
async def unclaim_ticket_cmd(ctx: commands.Context) -> None:
    """Release a ticket claim."""
    if not ticket_tool:
        await ctx.send("Ticket system not initialized.")
        return
    
    success, message = await ticket_tool.unclaim_ticket(ctx.channel, ctx.author)
    
    if success:
        await ctx.send(embed=discord.Embed(title="Ticket Unclaimed", description=message, color=discord.Color.orange()))
    else:
        await ctx.send(message)


@bot.hybrid_command(name="close", description="Close the current ticket")
@app_commands.describe(reason="Reason for closing")
async def close_ticket_cmd(ctx: commands.Context, *, reason: str = "No reason provided") -> None:
    """Close a ticket with optional reason."""
    if not ticket_tool:
        await ctx.send("Ticket system not initialized.")
        return
    
    ticket = data_manager.load_ticket_by_channel(ctx.channel.id)
    if not ticket:
        await ctx.send("This is not a ticket channel.")
        return
    
    # TicketRatingView handles closing after rating (or on timeout)
    view = TicketRatingView(ticket['ticket_id'], reason, ctx.channel, ctx.author)
    await ctx.send(
        "Please rate your support experience before the ticket closes (or skip to close immediately):",
        view=view
    )


@bot.hybrid_command(name="transcript", description="Generate a transcript of this ticket")
async def transcript_cmd(ctx: commands.Context) -> None:
    """Generate a transcript of the current ticket."""
    if not ticket_tool:
        await ctx.send("Ticket system not initialized.")
        return
    
    ticket = data_manager.load_ticket_by_channel(ctx.channel.id)
    if not ticket:
        await ctx.send("This is not a ticket channel.")
        return
    
    transcript = await ticket_tool._generate_transcript(ctx.channel, ticket, ctx.author)
    await ctx.send(embed=transcript['embed'], file=transcript['file'])


@bot.hybrid_command(name="ticketsettings", description="Configure ticket system settings")
@commands.has_permissions(manage_guild=True)
async def ticket_settings_cmd(ctx: commands.Context) -> None:
    """Open ticket settings configuration."""
    settings = data_manager.load_ticket_settings(ctx.guild.id) or {}
    
    embed = discord.Embed(title="Ticket System Settings", color=discord.Color.blurple())
    
    category_id = settings.get('category_id')
    category = ctx.guild.get_channel(category_id) if category_id else None
    
    transcripts_id = settings.get('transcripts_channel_id')
    transcripts = ctx.guild.get_channel(transcripts_id) if transcripts_id else None
    
    support_id = settings.get('support_role_id')
    support_role = ctx.guild.get_role(support_id) if support_id else None
    
    embed.add_field(name="Ticket Category", value=category.name if category else "Not Set", inline=True)
    embed.add_field(name="Ticket Transcripts Archive Channel", value=transcripts.mention if transcripts else "Not Set", inline=True)
    embed.add_field(name="Support Role", value=support_role.mention if support_role else "Not Set", inline=True)
    embed.add_field(name="Max Tickets/User", value=str(settings.get('max_tickets_per_user', 3)), inline=True)
    embed.add_field(name="Auto-Close Hours", value=str(settings.get('auto_close_hours', 24)), inline=True)
    embed.add_field(name="DM Transcripts", value="Yes" if settings.get('dm_transcripts', 1) else "No", inline=True)
    
    embed.set_footer(text="Use the modal to update settings")
    
    await ctx.send(embed=embed, view=TicketSettingsConfigView(ctx.guild.id))


class TicketSettingsConfigView(View):
    def __init__(self, guild_id: int):
        super().__init__(timeout=300)
        self.guild_id = guild_id
    
    @discord.ui.button(label="Set Category", style=discord.ButtonStyle.primary)
    async def set_category(self, interaction: discord.Interaction, button: Button) -> None:
        await interaction.response.send_modal(SetTicketCategoryModal(self.guild_id))
    
    @discord.ui.button(label="Set Transcripts", style=discord.ButtonStyle.primary)
    async def set_transcripts(self, interaction: discord.Interaction, button: Button) -> None:
        await interaction.response.send_modal(SetTranscriptsChannelModal(self.guild_id))
    
    @discord.ui.button(label="Set Support Role", style=discord.ButtonStyle.secondary)
    async def set_support_role(self, interaction: discord.Interaction, button: Button) -> None:
        await interaction.response.send_modal(SetSupportRoleModal(self.guild_id))


class SetTicketCategoryModal(Modal, title="Set Ticket Category"):
    category_input = TextInput(label="Category ID", placeholder="Enter category channel ID")
    
    def __init__(self, guild_id: int):
        super().__init__()
        self.guild_id = guild_id
        settings = data_manager.load_ticket_settings(guild_id) or {}
        if settings.get('category_id'):
            self.category_input.default = str(settings['category_id'])
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        try:
            category_id = int(self.category_input.value)
            category = interaction.guild.get_channel(category_id)
            if not category or not isinstance(category, discord.CategoryChannel):
                await interaction.response.send_message("Invalid category ID.", ephemeral=True)
                return
            settings = data_manager.load_ticket_settings(self.guild_id) or {'guild_id': self.guild_id}
            settings['category_id'] = category_id
            settings['updated_at'] = datetime.now(timezone.utc).isoformat()
            data_manager.save_ticket_settings(settings)
            await interaction.response.send_message(f"Ticket category set to **{category.name}**", ephemeral=True)
        except ValueError:
            await interaction.response.send_message("Please enter a valid number.", ephemeral=True)


class SetTranscriptsChannelModal(Modal, title="Set Transcripts Channel"):
    channel_input = TextInput(label="Channel ID", placeholder="Enter transcripts channel ID")
    
    def __init__(self, guild_id: int):
        super().__init__()
        self.guild_id = guild_id
        settings = data_manager.load_ticket_settings(guild_id) or {}
        if settings.get('transcripts_channel_id'):
            self.channel_input.default = str(settings['transcripts_channel_id'])
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        try:
            channel_id = int(self.channel_input.value)
            channel = interaction.guild.get_channel(channel_id)
            if not channel or not isinstance(channel, discord.TextChannel):
                await interaction.response.send_message("Invalid channel ID.", ephemeral=True)
                return
            settings = data_manager.load_ticket_settings(self.guild_id) or {'guild_id': self.guild_id}
            settings['transcripts_channel_id'] = channel_id
            settings['updated_at'] = datetime.now(timezone.utc).isoformat()
            data_manager.save_ticket_settings(settings)
            await interaction.response.send_message(f"Transcripts channel set to {channel.mention}", ephemeral=True)
        except ValueError:
            await interaction.response.send_message("Please enter a valid number.", ephemeral=True)


class SetSupportRoleModal(Modal, title="Set Support Role"):
    role_input = TextInput(label="Role ID", placeholder="Enter support role ID")
    
    def __init__(self, guild_id: int):
        super().__init__()
        self.guild_id = guild_id
        settings = data_manager.load_ticket_settings(guild_id) or {}
        if settings.get('support_role_id'):
            self.role_input.default = str(settings['support_role_id'])
    
    async def on_submit(self, interaction: discord.Interaction) -> None:
        try:
            role_id = int(self.role_input.value)
            role = interaction.guild.get_role(role_id)
            if not role:
                await interaction.response.send_message("Invalid role ID.", ephemeral=True)
                return
            settings = data_manager.load_ticket_settings(self.guild_id) or {'guild_id': self.guild_id}
            settings['support_role_id'] = role_id
            settings['updated_at'] = datetime.now(timezone.utc).isoformat()
            data_manager.save_ticket_settings(settings)
            await interaction.response.send_message(f"Support role set to {role.mention}", ephemeral=True)
        except ValueError:
            await interaction.response.send_message("Please enter a valid number.", ephemeral=True)


@bot.hybrid_command(name="ticketblacklist", description="Blacklist a user from creating tickets")
@commands.has_permissions(manage_guild=True)
@app_commands.describe(user="User to blacklist", reason="Reason for blacklist")
async def ticket_blacklist_cmd(ctx: commands.Context, user: discord.Member, *, reason: str = "No reason provided") -> None:
    """Blacklist a user from creating tickets."""
    import uuid
    blacklist_data = {
        'blacklist_id': str(uuid.uuid4())[:8],
        'guild_id': ctx.guild.id,
        'user_id': user.id,
        'reason': reason,
        'blacklisted_by': ctx.author.id,
        'blacklisted_at': datetime.now(timezone.utc).isoformat(),
        'is_active': 1
    }
    data_manager.save_ticket_blacklist(blacklist_data)
    await ctx.send(embed=discord.Embed(
        title="User Blacklisted",
        description=f"{user.mention} has been blacklisted from creating tickets.\n**Reason:** {reason}",
        color=discord.Color.red()
    ))


@bot.hybrid_command(name="ticketunblacklist", description="Remove a user from the ticket blacklist")
@commands.has_permissions(manage_guild=True)
@app_commands.describe(user="User to unblacklist")
async def ticket_unblacklist_cmd(ctx: commands.Context, user: discord.Member) -> None:
    """Remove a user from the ticket blacklist."""
    success = data_manager.remove_ticket_blacklist(ctx.guild.id, user.id)
    if success:
        await ctx.send(f"{user.mention} has been removed from the blacklist.")
    else:
        await ctx.send(f"{user.mention} is not blacklisted.")


@bot.hybrid_command(name="tickets", description="View open tickets")
@commands.has_permissions(manage_channels=True)
async def view_tickets_cmd(ctx: commands.Context) -> None:
    tickets = data_manager.load_tickets_by_guild(ctx.guild.id, 'open')
    if not tickets:
        await ctx.send("No open tickets.")
        return

    # Filter out orphaned tickets (channel no longer exists)
    valid_tickets = []
    for ticket in tickets:
        channel = ctx.guild.get_channel(ticket['channel_id'])
        if channel:
            valid_tickets.append(ticket)
        else:
            # Auto-close orphaned tickets
            ticket['status'] = 'closed'
            ticket['close_reason'] = 'Channel no longer exists (auto-cleaned)'
            ticket['closed_at'] = datetime.now(timezone.utc).isoformat()
            data_manager.save_ticket(ticket)

    if not valid_tickets:
        await ctx.send("No open tickets with active channels.")
        return

    embed = discord.Embed(title="Open Tickets", description=f"Found **{len(valid_tickets)}** open ticket(s)", color=discord.Color.blurple())
    for ticket in valid_tickets[:10]:
        creator = ctx.guild.get_member(ticket['creator_id'])
        creator_name = creator.mention if creator else f"<@{ticket['creator_id']}>"
        claimed = ""
        if ticket.get('claimed_by'):
            claimer = ctx.guild.get_member(ticket['claimed_by'])
            claimed = f"\nClaimed: {claimer.mention if claimer else 'Unknown'}"
        embed.add_field(
            name=f"Ticket #{ticket['ticket_id']}",
            value=f"Creator: {creator_name}\nCategory: {ticket.get('category', 'General')}\nChannel: <#{ticket['channel_id']}>{claimed}",
            inline=False
        )
    await ctx.send(embed=embed)


# =============================================================================
# --- NEW TICKET TOOL FEATURE COMMANDS ---
# =============================================================================

@bot.hybrid_command(name="add", description="Add a user to the current ticket")
@commands.has_permissions(manage_channels=True)
@app_commands.describe(user="User to add to this ticket")
async def ticket_add_cmd(ctx: commands.Context, user: discord.Member) -> None:
    ticket = data_manager.load_ticket_by_channel(ctx.channel.id)
    if not ticket:
        await ctx.send("This is not a ticket channel.", ephemeral=True)
        return
    await ctx.channel.set_permissions(
        user,
        view_channel=True,
        send_messages=True,
        read_message_history=True,
        attach_files=True
    )
    await ctx.send(embed=discord.Embed(
        description=f"✅ {user.mention} has been added to the ticket.",
        color=discord.Color.green()
    ))
    logging.info(f"[Tickets] {ctx.author} added {user} to ticket {ticket['ticket_id']}")


@bot.hybrid_command(name="remove", description="Remove a user from the current ticket")
@commands.has_permissions(manage_channels=True)
@app_commands.describe(user="User to remove from this ticket")
async def ticket_remove_cmd(ctx: commands.Context, user: discord.Member) -> None:
    ticket = data_manager.load_ticket_by_channel(ctx.channel.id)
    if not ticket:
        await ctx.send("This is not a ticket channel.", ephemeral=True)
        return
    # Don't allow removing the ticket creator
    if user.id == ticket.get('creator_id'):
        await ctx.send("You cannot remove the ticket creator.", ephemeral=True)
        return
    await ctx.channel.set_permissions(user, overwrite=None)
    await ctx.send(embed=discord.Embed(
        description=f"✅ {user.mention} has been removed from the ticket.",
        color=discord.Color.orange()
    ))
    logging.info(f"[Tickets] {ctx.author} removed {user} from ticket {ticket['ticket_id']}")


@bot.hybrid_command(name="rename", description="Rename the current ticket channel")
@commands.has_permissions(manage_channels=True)
@app_commands.describe(name="New channel name (no spaces)")
async def ticket_rename_cmd(ctx: commands.Context, *, name: str) -> None:
    ticket = data_manager.load_ticket_by_channel(ctx.channel.id)
    if not ticket:
        await ctx.send("This is not a ticket channel.", ephemeral=True)
        return
    clean_name = ''.join(c if c.isalnum() or c == '-' else '-' for c in name.lower())[:50]
    old_name = ctx.channel.name
    await ctx.channel.edit(name=clean_name)
    await ctx.send(embed=discord.Embed(
        description=f"✅ Channel renamed from `{old_name}` → `{clean_name}`",
        color=discord.Color.green()
    ))


@bot.hybrid_command(name="move", description="Move the ticket to a different panel category")
@commands.has_permissions(manage_channels=True)
@app_commands.describe(panel_id="Panel ID to move this ticket under")
async def ticket_move_cmd(ctx: commands.Context, panel_id: str) -> None:
    ticket = data_manager.load_ticket_by_channel(ctx.channel.id)
    if not ticket:
        await ctx.send("This is not a ticket channel.", ephemeral=True)
        return
    panel = data_manager.load_ticket_panel(panel_id)
    if not panel or panel['guild_id'] != ctx.guild.id:
        await ctx.send(f"Panel `{panel_id}` not found in this server.", ephemeral=True)
        return
    category_id = panel.get('category_id')
    if not category_id:
        await ctx.send("That panel has no category set.", ephemeral=True)
        return
    category = ctx.guild.get_channel(category_id)
    if not category:
        await ctx.send("Category channel not found.", ephemeral=True)
        return
    await ctx.channel.edit(category=category)
    ticket['panel_id'] = panel_id
    ticket['category'] = panel.get('name', 'General')
    data_manager.save_ticket(ticket)
    await ctx.send(embed=discord.Embed(
        description=f"✅ Ticket moved to **{panel.get('name', 'Unknown')}** (category: {category.name})",
        color=discord.Color.green()
    ))


@bot.hybrid_command(name="note", description="Add a private staff note to this ticket")
@commands.has_permissions(manage_channels=True)
@app_commands.describe(content="Note content (only staff can view these)")
async def ticket_note_cmd(ctx: commands.Context, *, content: str) -> None:
    ticket = data_manager.load_ticket_by_channel(ctx.channel.id)
    if not ticket:
        await ctx.send("This is not a ticket channel.", ephemeral=True)
        return
    import uuid
    note = {
        'note_id': str(uuid.uuid4())[:8],
        'ticket_id': ticket['ticket_id'],
        'guild_id': ctx.guild.id,
        'author_id': ctx.author.id,
        'content': content,
        'created_at': datetime.now(timezone.utc).isoformat(),
    }
    data_manager.save_ticket_note(note)
    await ctx.send(embed=discord.Embed(
        title="📝 Note Saved",
        description=content,
        color=discord.Color.yellow(),
        timestamp=datetime.now(timezone.utc)
    ).set_footer(text=f"By {ctx.author.display_name} • ID: {note['note_id']}"), ephemeral=True)


@bot.hybrid_command(name="notes", description="View all staff notes for this ticket")
@commands.has_permissions(manage_channels=True)
async def ticket_notes_cmd(ctx: commands.Context) -> None:
    ticket = data_manager.load_ticket_by_channel(ctx.channel.id)
    if not ticket:
        await ctx.send("This is not a ticket channel.", ephemeral=True)
        return
    notes = data_manager.load_ticket_notes(ticket['ticket_id'])
    if not notes:
        await ctx.send("No notes found for this ticket.", ephemeral=True)
        return
    embed = discord.Embed(
        title=f"📝 Notes for Ticket #{ticket['ticket_id']}",
        color=discord.Color.yellow(),
        timestamp=datetime.now(timezone.utc)
    )
    for note in notes[:10]:
        author = ctx.guild.get_member(note['author_id'])
        author_name = author.display_name if author else f"<@{note['author_id']}>"
        created = note['created_at'][:16].replace('T', ' ')
        embed.add_field(
            name=f"Note {note['note_id']} — {author_name} at {created}",
            value=note['content'][:1024],
            inline=False
        )
    await ctx.send(embed=embed, ephemeral=True)


@bot.hybrid_command(name="priority", description="Set the priority of the current ticket")
@commands.has_permissions(manage_channels=True)
@app_commands.describe(level="Priority level: low, normal, high, urgent")
@app_commands.choices(level=[
    app_commands.Choice(name="🟢 Low", value="low"),
    app_commands.Choice(name="🔵 Normal", value="normal"),
    app_commands.Choice(name="🟠 High", value="high"),
    app_commands.Choice(name="🔴 Urgent", value="urgent"),
])
async def ticket_priority_cmd(ctx: commands.Context, level: str) -> None:
    ticket = data_manager.load_ticket_by_channel(ctx.channel.id)
    if not ticket:
        await ctx.send("This is not a ticket channel.", ephemeral=True)
        return
    ticket['priority'] = level
    data_manager.save_ticket(ticket)
    emoji = PRIORITY_EMOJIS.get(level, '')
    color = PRIORITY_COLORS.get(level, discord.Color.blue())
    await ctx.send(embed=discord.Embed(
        description=f"{emoji} Ticket priority set to **{level.capitalize()}** by {ctx.author.mention}",
        color=color,
        timestamp=datetime.now(timezone.utc)
    ))


@bot.hybrid_command(name="reopen", description="Reopen a closed ticket")
@commands.has_permissions(manage_channels=True)
@app_commands.describe(ticket_id="The ticket ID to reopen")
async def ticket_reopen_cmd(ctx: commands.Context, ticket_id: str) -> None:
    """Reopen a closed ticket by recreating its channel."""
    if not ticket_tool:
        await ctx.send("Ticket system not initialized.")
        return
    ticket = data_manager.load_ticket(ticket_id)
    if not ticket:
        await ctx.send(f"Ticket `{ticket_id}` not found.")
        return
    if ticket['guild_id'] != ctx.guild.id:
        await ctx.send("That ticket does not belong to this server.")
        return
    if ticket.get('status') == 'open':
        existing_channel = ctx.guild.get_channel(ticket['channel_id'])
        if existing_channel:
            await ctx.send(f"That ticket is already open: {existing_channel.mention}")
            return

    # Rebuild the channel
    creator = ctx.guild.get_member(ticket['creator_id'])
    if not creator:
        await ctx.send("Cannot reopen — the original ticket creator is no longer in the server.")
        return

    # Use the panel if available, otherwise use default settings
    panel = data_manager.load_ticket_panel(ticket.get('panel_id', '')) or {}
    settings = data_manager.load_ticket_settings(ctx.guild.id) or {}
    category_id = panel.get('category_id') or settings.get('category_id')
    support_role_id = panel.get('support_role_id') or settings.get('support_role_id')

    overwrites = {
        ctx.guild.default_role: discord.PermissionOverwrite(view_channel=False),
        creator: discord.PermissionOverwrite(view_channel=True, send_messages=True, read_message_history=True, attach_files=True),
        ctx.guild.me: discord.PermissionOverwrite(view_channel=True, send_messages=True, manage_channels=True, read_message_history=True),
    }
    if support_role_id:
        role = ctx.guild.get_role(support_role_id)
        if role:
            overwrites[role] = discord.PermissionOverwrite(view_channel=True, send_messages=True, read_message_history=True, attach_files=True)

    category = ctx.guild.get_channel(category_id) if category_id else None
    channel_name = f"ticket-{creator.display_name}".lower()[:50]
    channel_name = ''.join(c if c.isalnum() or c == '-' else '-' for c in channel_name)

    try:
        new_channel = await ctx.guild.create_text_channel(
            channel_name,
            category=category,
            overwrites=overwrites,
            topic=f"Ticket {ticket_id} (reopened) - {creator}"
        )
    except Exception as e:
        await ctx.send(f"Failed to create channel: {e}")
        return

    ticket['channel_id'] = new_channel.id
    ticket['status'] = 'open'
    ticket['closed_at'] = None
    ticket['closed_by'] = None
    ticket['close_reason'] = None
    data_manager.save_ticket(ticket)

    control_view = TicketControlView(ticket_id)
    await new_channel.send(
        embed=discord.Embed(
            title=f"🔓 Ticket Reopened — #{ticket_id}",
            description=f"This ticket was reopened by {ctx.author.mention}.\n{creator.mention} your ticket has been reopened.",
            color=discord.Color.green(),
            timestamp=datetime.now(timezone.utc)
        ),
        view=control_view
    )
    await ctx.send(f"Ticket `{ticket_id}` reopened: {new_channel.mention}")
    logging.info(f"[Tickets] {ctx.author} reopened ticket {ticket_id}")


@bot.hybrid_command(name="ticketstats", description="View ticket statistics for this server")
@commands.has_permissions(manage_channels=True)
async def ticket_stats_cmd(ctx: commands.Context) -> None:
    stats = data_manager.load_ticket_stats(ctx.guild.id)
    embed = discord.Embed(
        title=f"📊 Ticket Statistics — {ctx.guild.name}",
        color=discord.Color.blurple(),
        timestamp=datetime.now(timezone.utc)
    )
    embed.add_field(name="Total Tickets", value=str(stats['total']), inline=True)
    embed.add_field(name="Open", value=str(stats['open']), inline=True)
    embed.add_field(name="Closed", value=str(stats['closed']), inline=True)

    avg_rating = f"⭐ {stats['avg_rating']}" if stats['avg_rating'] else "No ratings yet"
    embed.add_field(name="Avg Rating", value=avg_rating, inline=True)

    avg_close = f"{stats['avg_close_hours']}h" if stats['avg_close_hours'] is not None else "N/A"
    embed.add_field(name="Avg Close Time", value=avg_close, inline=True)

    priorities = stats.get('priorities', {})
    if priorities:
        prio_text = "\n".join(
            f"{PRIORITY_EMOJIS.get(k, '')} {k.capitalize()}: {v}"
            for k, v in priorities.items()
        )
    else:
        prio_text = "No open tickets"
    embed.add_field(name="Open by Priority", value=prio_text, inline=False)

    await ctx.send(embed=embed)


@bot.hybrid_command(name="dbcleanup", description="Scan the database and remove stale, invalid, or unused data")
@commands.has_permissions(administrator=True)
async def dbcleanup_cmd(ctx: commands.Context) -> None:
    """
    Scans every ticket-related table and cross-checks against live Discord state.
    Removes / closes anything that no longer has a valid corresponding Discord object.

    What it checks:
      - Panels      : is the Discord channel and message still there?
      - Open tickets : does the ticket channel still exist in Discord?
      - Closed tickets: are their answers, notes, messages, transcripts still linked to a real ticket?
      - Blacklist    : are entries expired or already inactive?
      - Warnings     : are there deactivated warning rows taking up space?
      - Invite History: are there archived invite links taking up space?
    """
    await ctx.defer()

    status_msg = await ctx.send(
        embed=discord.Embed(
            title="🔍 Database Cleanup — Scanning...",
            description="Checking every table against live Discord state. Please wait.",
            color=discord.Color.yellow()
        )
    )

    report_lines: List[str] = []

    # ─── 1. PANELS ──────────────────────────────────────────────────────────────
    all_panels = data_manager.load_all_ticket_panels()
    valid_panel_ids: set = set()
    panels_deactivated = 0

    for panel in all_panels:
        if not panel.get('is_active'):
            continue  # Already inactive, skip
        guild = bot.get_guild(panel['guild_id'])
        if not guild:
            # Bot no longer in that guild — deactivate
            panel['is_active'] = 0
            data_manager.save_ticket_panel(panel)
            panels_deactivated += 1
            continue
        channel = guild.get_channel(panel.get('channel_id', 0))
        if not channel:
            panel['is_active'] = 0
            data_manager.save_ticket_panel(panel)
            panels_deactivated += 1
            continue
        # Try to verify the panel message still exists
        msg_id = panel.get('message_id')
        if msg_id:
            try:
                await channel.fetch_message(msg_id)
            except (discord.NotFound, discord.Forbidden):
                panel['is_active'] = 0
                data_manager.save_ticket_panel(panel)
                panels_deactivated += 1
                continue
        valid_panel_ids.add(panel['panel_id'])

    if panels_deactivated:
        report_lines.append(f"🗂️ **Panels** — deactivated **{panels_deactivated}** (channel/message gone)")
    else:
        report_lines.append("🗂️ **Panels** — ✅ all active panels valid")

    # ─── 2. TICKETS ──────────────────────────────────────────────────────────
    all_tickets = data_manager.load_all_tickets()
    valid_ticket_ids: set = set()
    closed_ticket_ids: set = set()
    orphaned_open_ticket_ids: set = set()

    for ticket in all_tickets:
        if ticket.get('status') == 'closed':
            # All closed tickets get purged — they're done, no longer needed
            closed_ticket_ids.add(ticket['ticket_id'])
        else:
            # Open ticket — check if its Discord channel still exists
            valid_ticket_ids.add(ticket['ticket_id'])
            guild = bot.get_guild(ticket['guild_id'])
            if not guild:
                orphaned_open_ticket_ids.add(ticket['ticket_id'])
                continue
            channel = guild.get_channel(ticket.get('channel_id', 0))
            if not channel:
                orphaned_open_ticket_ids.add(ticket['ticket_id'])

    if closed_ticket_ids:
        report_lines.append(
            f"🎫 **Closed Tickets** — permanently deleting **{len(closed_ticket_ids)}** "
            f"closed ticket(s) and their linked cache data (answers, notes, messages). "
            f"Transcripts are PRESERVED for historical reference."
        )
    else:
        report_lines.append("🎫 **Closed Tickets** — ✅ none to remove")

    if orphaned_open_ticket_ids:
        report_lines.append(
            f"⚠️ **Orphaned Open Tickets** — marking **{len(orphaned_open_ticket_ids)}** "
            f"as closed (Discord channel no longer exists)"
        )

    # ─── 3. BLACKLIST ────────────────────────────────────────────────────────
    all_blacklist = data_manager.load_all_ticket_blacklist()
    expired_blacklist_ids: set = set()
    now_utc = datetime.now(timezone.utc)

    for entry in all_blacklist:
        if not entry.get('is_active'):
            expired_blacklist_ids.add(entry['blacklist_id'])
            continue
        expires_at = entry.get('expires_at')
        if expires_at:
            try:
                exp_dt = datetime.fromisoformat(expires_at.replace('Z', '+00:00'))
                if now_utc > exp_dt:
                    expired_blacklist_ids.add(entry['blacklist_id'])
            except Exception:
                pass

    if expired_blacklist_ids:
        report_lines.append(
            f"🚫 **Ticket Blacklist** — removing **{len(expired_blacklist_ids)}** "
            f"expired/inactive entries"
        )
    else:
        report_lines.append("🚫 **Ticket Blacklist** — ✅ no expired entries")

    # ─── 4. COUNT ORPHANED CHILD ROWS (preview before deleting) ─────────────
    cursor = data_manager._connection.cursor()

    def count_orphans(table: str, fk_col: str, valid_ids: set) -> int:
        if not valid_ids:
            cursor.execute(f'SELECT COUNT(*) FROM {table}')
        else:
            ph = ','.join('?' * len(valid_ids))
            cursor.execute(
                f'SELECT COUNT(*) FROM {table} WHERE {fk_col} NOT IN ({ph})',
                list(valid_ids)
            )
        return cursor.fetchone()[0]

    orphan_answers    = count_orphans('ticket_answers',     'ticket_id', valid_ticket_ids)
    orphan_notes      = count_orphans('ticket_notes',       'ticket_id', valid_ticket_ids)
    orphan_messages   = count_orphans('ticket_messages',    'ticket_id', valid_ticket_ids)
    orphan_transcripts= count_orphans('ticket_transcripts', 'ticket_id', valid_ticket_ids)
    orphan_questions  = count_orphans('ticket_questions',   'panel_id',  valid_panel_ids)

    cursor.execute('SELECT COUNT(*) FROM warnings WHERE is_active = 0')
    inactive_warnings = cursor.fetchone()[0]

    child_total = orphan_answers + orphan_notes + orphan_messages + orphan_questions

    if child_total:
        report_lines.append(
            f"🗑️ **Orphaned rows** found (will be deleted):\n"
            f"  • Answers: {orphan_answers}\n"
            f"  • Notes: {orphan_notes}\n"
            f"  • Cached messages: {orphan_messages}\n"
            f"  • Questions: {orphan_questions}\n"
            f"  • Transcripts: {orphan_transcripts} (PRESERVED — not deleted)"
        )
    else:
        report_lines.append(
            f"🗑️ **Orphaned child rows** — ✅ none found to delete "
            f"({orphan_transcripts} transcript(s) preserved)"
        )

    if inactive_warnings:
        report_lines.append(f"⚠️ **Warnings** — removing **{inactive_warnings}** deactivated rows")
    else:
        report_lines.append("⚠️ **Warnings** — ✅ no inactive rows")

    # ─── 5. INVITE HISTORY ────────────────────────────────────────────────────
    cursor.execute('SELECT COUNT(*) FROM invite_history WHERE guild_id = ?', (ctx.guild.id,))
    archived_invites_count = cursor.fetchone()[0]
    clear_invite_history_flag = archived_invites_count > 0

    if clear_invite_history_flag:
        report_lines.append(
            f"📜 **Invite History** — permanently deleting **{archived_invites_count}** "
            f"archived invite record(s) to free up space."
        )
    else:
        report_lines.append("📜 **Invite History** — ✅ none to remove")

    # ─── 6. NOTHING TO DO? ──────────────────────────────────────────────────
    nothing_to_do = (
        panels_deactivated == 0
        and len(closed_ticket_ids) == 0
        and len(orphaned_open_ticket_ids) == 0
        and len(expired_blacklist_ids) == 0
        and child_total == 0
        and inactive_warnings == 0
        and not clear_invite_history_flag
    )

    if nothing_to_do:
        await status_msg.edit(embed=discord.Embed(
            title="✅ Database Cleanup — Nothing to clean",
            description="Every table was checked. All rows are valid and in use.",
            color=discord.Color.green(),
            timestamp=datetime.now(timezone.utc)
        ))
        return

    # ─── 7. CONFIRM VIEW ────────────────────────────────────────────────────
    preview_embed = discord.Embed(
        title="🔍 Database Cleanup — Review",
        description="\n".join(report_lines),
        color=discord.Color.orange(),
        timestamp=datetime.now(timezone.utc)
    )
    preview_embed.set_footer(text="Press Confirm to permanently apply these changes, or Cancel to abort.")

    confirm_view = DBCleanupConfirmView(
        ctx.author.id,
        valid_ticket_ids=valid_ticket_ids,
        valid_panel_ids=valid_panel_ids,
        orphaned_open_ticket_ids=orphaned_open_ticket_ids,
        expired_blacklist_ids=expired_blacklist_ids,
        closed_ticket_ids=closed_ticket_ids,
        report_lines=report_lines,
        guild_id=ctx.guild.id,
        clear_invite_history=clear_invite_history_flag,
    )
    await status_msg.edit(embed=preview_embed, view=confirm_view)


class DBCleanupConfirmView(View):
    def __init__(
        self,
        user_id: int,
        valid_ticket_ids: set,
        valid_panel_ids: set,
        orphaned_open_ticket_ids: set,
        expired_blacklist_ids: set,
        closed_ticket_ids: set,
        report_lines: List[str],
        guild_id: int,
        clear_invite_history: bool = False,
    ):
        super().__init__(timeout=120)
        self.user_id = user_id
        self.valid_ticket_ids = valid_ticket_ids
        self.valid_panel_ids = valid_panel_ids
        self.orphaned_open_ticket_ids = orphaned_open_ticket_ids
        self.expired_blacklist_ids = expired_blacklist_ids
        self.closed_ticket_ids = closed_ticket_ids
        self.report_lines = report_lines
        self.guild_id = guild_id
        self.clear_invite_history = clear_invite_history

    @discord.ui.button(label="✅ Confirm Cleanup", style=discord.ButtonStyle.danger)
    async def confirm(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Only the person who ran this command can confirm.", ephemeral=True)
            return

        await interaction.response.defer()

        counts = data_manager.purge_stale_data(
            valid_ticket_ids=self.valid_ticket_ids,
            valid_panel_ids=self.valid_panel_ids,
            orphaned_open_ticket_ids=self.orphaned_open_ticket_ids,
            expired_blacklist_ids=self.expired_blacklist_ids,
            closed_ticket_ids=self.closed_ticket_ids,
        )

        # Clear invite history if the flag was set
        if self.clear_invite_history:
            cleared_invites = data_manager.clear_invite_history(guild_id=self.guild_id)
            counts['invite_history'] = cleared_invites
        else:
            counts['invite_history'] = 0

        total_removed = sum(counts.values())

        result_lines = self.report_lines + [
            "",
            "**Rows removed:**",
            f"  • Closed tickets deleted: {counts.get('closed_tickets', 0)}",
            f"  • Answers deleted: {counts.get('closed_answers', 0) + counts.get('orphan_answers', 0)}",
            f"  • Notes deleted: {counts.get('closed_notes', 0) + counts.get('orphan_notes', 0)}",
            f"  • Cached messages deleted: {counts.get('closed_messages', 0) + counts.get('orphan_messages', 0)}",
            f"  • Transcripts deleted: 0 (PRESERVED — kept for historical reference)",
            f"  • Questions deleted: {counts.get('orphan_questions', 0)}",
            f"  • Open tickets closed (orphaned): {counts.get('orphaned_tickets_closed', 0)}",
            f"  • Blacklist entries removed: {counts.get('ticket_blacklist', 0)}",
            f"  • Inactive warnings removed: {counts.get('warnings', 0)}",
            f"  • Archived invite records deleted: {counts.get('invite_history', 0)}",
            "",
            f"**Total rows cleaned: {total_removed}**",
        ]

        for child in self.children:
            child.disabled = True

        await interaction.edit_original_response(
            embed=discord.Embed(
                title="✅ Database Cleanup — Complete",
                description="\n".join(result_lines),
                color=discord.Color.green(),
                timestamp=datetime.now(timezone.utc)
            ),
            view=self
        )
        logging.info(f"[DBCleanup] Cleanup run by {interaction.user}: {counts}")
        self.stop()

    @discord.ui.button(label="❌ Cancel", style=discord.ButtonStyle.secondary)
    async def cancel(self, interaction: discord.Interaction, button: Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("Only the person who ran this command can cancel.", ephemeral=True)
            return
        for child in self.children:
            child.disabled = True
        await interaction.response.edit_message(
            embed=discord.Embed(
                title="❌ Database Cleanup — Cancelled",
                description="No changes were made.",
                color=discord.Color.red(),
                timestamp=datetime.now(timezone.utc)
            ),
            view=self
        )
        self.stop()

# GIVEAWAY COMMANDS (V2 Enhancement)
# =============================================================================
@bot.hybrid_command(name="giveaway", description="Create a giveaway")
@commands.has_permissions(manage_guild=True)
@app_commands.describe(duration="Duration in hours", winners="Number of winners", prize="The prize")
async def giveaway_cmd(ctx: commands.Context, duration: int, winners: int, *, prize: str) -> None:
    if not config.enable_giveaways:
        await ctx.send("Giveaway system is disabled.")
        return
    
    if winners > config.limits.max_giveaway_winners:
        await ctx.send(f"Maximum {config.limits.max_giveaway_winners} winners allowed.")
        return
    
    import uuid
    giveaway_id = str(uuid.uuid4())[:8]
    
    giveaway = {
        'giveaway_id': giveaway_id,
        'message_id': 0,
        'channel_id': ctx.channel.id,
        'guild_id': ctx.guild.id,
        'host_id': ctx.author.id,
        'prize': prize,
        'winner_count': winners,
        'entries': [],
        'status': 'active',
        'created_at': datetime.now(timezone.utc).isoformat(),
        'ends_at': (datetime.now(timezone.utc) + timedelta(hours=duration)).isoformat()
    }
    
    embed = EmbedBuilder.giveaway(
        prize,
        f"**Hosted by:** {ctx.author.mention}\n"
        f"**Winners:** {winners}\n"
        f"**Ends:** <t:{int(datetime.fromisoformat(giveaway['ends_at']).timestamp())}:R>\n"
        f"**Entries:** 0\n\n"
        f"Click the button below to enter!"
    )
    
    view = GiveawayView(giveaway_id)
    message = await ctx.send(embed=embed, view=view)
    
    giveaway['message_id'] = message.id
    giveaways_data[giveaway_id] = giveaway
    save_giveaways_data()
    
    logging.info(f"[Giveaway] Created by {ctx.author}: {prize}")


@bot.hybrid_command(name="endgiveaway", description="End a giveaway early")
@commands.has_permissions(manage_guild=True)
@app_commands.describe(giveaway_id="The giveaway ID to end")
async def endgiveaway_cmd(ctx: commands.Context, giveaway_id: str) -> None:
    if giveaway_id not in giveaways_data:
        await ctx.send("Giveaway not found.")
        return
    
    await end_giveaway(giveaway_id)
    await ctx.send(f"Giveaway `{giveaway_id}` has been ended.")


# --- LEVEL COMMANDS (V2 Enhancement) ---
@bot.hybrid_command(name="level", description="View your level")
@app_commands.describe(member="Member to check")
async def level_cmd(ctx: commands.Context, member: Optional[discord.Member] = None) -> None:
    if not config.enable_leveling:
        await ctx.send("Leveling system is disabled.")
        return
    
    member = member or ctx.author
    key = (member.id, ctx.guild.id)
    level_data = levels_data.get(key, {'xp': 0, 'level': 0, 'total_messages': 0})
    
    embed = EmbedBuilder.level(member, level_data)
    await ctx.send(embed=embed)


@bot.hybrid_command(name="leaderboard", description="View the server leaderboard")
async def leaderboard_cmd(ctx: commands.Context) -> None:
    if not config.enable_leveling:
        await ctx.send("Leveling system is disabled.")
        return
    
    # Get top 10 users in this guild
    guild_levels = [(uid, data) for (uid, gid), data in levels_data.items() if gid == ctx.guild.id]
    guild_levels.sort(key=lambda x: x[1].get('xp', 0), reverse=True)
    
    if not guild_levels:
        await ctx.send("No data yet. Start chatting to earn XP!")
        return
    
    embed = EmbedBuilder.info("🏆 Leaderboard", "")
    
    medals = ["🥇", "🥈", "🥉"]
    
    for idx, (user_id, data) in enumerate(guild_levels[:10]):
        medal = medals[idx] if idx < 3 else f"#{idx + 1}"
        user = ctx.guild.get_member(user_id)
        name = user.display_name if user else f"User {user_id}"
        
        embed.add_field(
            name=f"{medal} {name}",
            value=f"Level {data.get('level', 0)} • {data.get('xp', 0):,} XP",
            inline=False
        )
    
    await ctx.send(embed=embed)


# --- INFORMATION COMMANDS ---
@bot.hybrid_command()
async def userinfo(ctx: commands.Context, member: discord.Member) -> None:
    embed = discord.Embed(title=f"{member.name}'s Info", color=discord.Color.blue())
    embed.add_field(name="ID", value=member.id)
    embed.add_field(name="Joined", value=member.joined_at)
    embed.add_field(name="Top Role", value=member.top_role)
    await ctx.send(embed=embed)


@bot.hybrid_command()
async def serverinfo(ctx: commands.Context) -> None:
    try:
        guild = ctx.guild
        online = sum(1 for m in guild.members if m.status != discord.Status.offline)
        bots = sum(1 for m in guild.members if m.bot)
        humans = guild.member_count - bots
        
        embed = discord.Embed(title=f"{guild.name} Server Info", color=discord.Color.blurple())
        embed.add_field(name="Server ID", value=guild.id, inline=True)
        embed.add_field(name="Owner", value=guild.owner.mention, inline=True)
        embed.add_field(name="Created", value=guild.created_at.strftime("%B %d, %Y"), inline=True)
        embed.add_field(name=f"Members ({guild.member_count})", value=f"Online: {online}\nOffline: {guild.member_count - online}\nBots: {bots}\nHumans: {humans}", inline=False)
        embed.add_field(name=f"Channels ({len(guild.text_channels) + len(guild.voice_channels)})", value=f"Text: {len(guild.text_channels)}\nVoice: {len(guild.voice_channels)}", inline=True)
        embed.add_field(name="Other Stats", value=f"Roles: {len(guild.roles)}\nBoosts: {guild.premium_subscription_count}", inline=True)
        
        if guild.icon:
            embed.set_thumbnail(url=guild.icon.url)
        
        await ctx.send(embed=embed)
    except Exception as e:
        await ctx.send("An error occurred")
        logging.error(f"Error in serverinfo: {str(e)}")


@bot.hybrid_command()
async def membercount(ctx: commands.Context) -> None:
    member_role = ctx.guild.get_role(config.roles.member)
    if member_role is None:
        await ctx.send("Member role not found.")
        return
    members_with_role = [m for m in ctx.guild.members if member_role in m.roles]
    await ctx.send(f'There are {len(members_with_role)} members with the member role.')


@bot.hybrid_command()
async def ping(ctx: commands.Context) -> None:
    await ctx.send(f'Pong! {round(bot.latency * 1000)}ms')


@bot.hybrid_command()
async def uptime_cmd(ctx: commands.Context) -> None:
    await ctx.send(f"Bot uptime: {get_uptime()}")


# --- INVITE MANAGEMENT COMMANDS ---
@bot.hybrid_command(name="setupinvites", description="Set up the invite tracking system")
@commands.has_permissions(manage_guild=True)
async def setupinvites_cmd(ctx: commands.Context) -> None:
    global invite_manager
    
    if invite_manager.invite_message_id:
        try:
            old_channel = bot.get_channel(invite_manager.invite_channel_id)
            if old_channel:
                old_message = await old_channel.fetch_message(invite_manager.invite_message_id)
                await old_message.delete()
        except Exception:
            pass
    
    message = await invite_manager.post_initial_message(ctx.channel)
    new_invites = await invite_manager.generate_all_invites(ctx.guild)
    
    if not new_invites:
        await ctx.send("Failed to create invite links. Make sure I have permission to create invites.")
        return
    
    invite_manager.tracked_invites = new_invites
    invite_manager.save_data()
    await invite_manager.update_invite_message(ctx.channel)
    
    # Send the success message and delete it after 3 seconds
    await ctx.send(embed=EmbedBuilder.success("Invite System Setup", f"Created **{len(new_invites)}** invite links."), delete_after=3.0)
    logging.info(f"[InviteManager] Setup completed by {ctx.author} in channel {ctx.channel.name}")


@bot.hybrid_command(name="regenerateinvites", description="Regenerate all invite links")
@commands.has_permissions(manage_guild=True)
async def regenerateinvites_cmd(ctx: commands.Context) -> None:
    global invite_manager
    
    if not invite_manager.invite_channel_id:
        await ctx.send("Invite system not set up. Use `/setupinvites` first.")
        return
    
    channel = bot.get_channel(invite_manager.invite_channel_id)
    if not channel:
        await ctx.send("Invite channel not found.")
        return
    
    # Send the initial progress embed
    progress_embed = discord.Embed(
        title="🔄 Regenerating Invites",
        description="⏳ Deleting old invite links...",
        color=discord.Color.orange(),
        timestamp=datetime.now(timezone.utc)
    )
    progress_msg = await ctx.send(embed=progress_embed)
    
    deleted_count = 0
    try:
        guild_invites = await ctx.guild.invites()
        
        for invite in guild_invites:
            if invite.code in invite_manager.tracked_invites:
                try:
                    await invite.delete(reason="Regenerating invite links")
                    deleted_count += 1
                except Exception as e:
                    logging.warning(f"[InviteManager] Could not delete invite {invite.code}: {e}")
    except discord.Forbidden:
        progress_embed.description = "❌ I don't have permission to manage invites."
        progress_embed.color = discord.Color.red()
        await progress_msg.edit(embed=progress_embed)
        return
    except Exception as e:
        progress_embed.description = f"❌ Error deleting old invites: {e}"
        progress_embed.color = discord.Color.red()
        await progress_msg.edit(embed=progress_embed)
        return

    # Update progress: Generating new invites
    progress_embed.description = "⏳ Generating new invite links..."
    await progress_msg.edit(embed=progress_embed)

    # Archive the outgoing batch FIRST so the "View Previous Invites" button
    # can show what was just replaced. Non-fatal if it fails.
    if invite_manager.tracked_invites:
        try:
            archived = invite_manager.archive_current_batch(ctx.guild.id, ctx.author.id)
            logging.info(f"[InviteManager] Archived {archived} old invite(s) to history before regen")
        except Exception as e:
            logging.warning(f"[InviteManager] Could not archive batch before regen: {e}")

    new_invites = await invite_manager.generate_all_invites(ctx.guild)
    
    if not new_invites:
        progress_embed.description = "❌ Failed to create new invites. Make sure I have the `Create Invite` permission."
        progress_embed.color = discord.Color.red()
        await progress_msg.edit(embed=progress_embed)
        return

    # Update progress: Updating tracking message
    progress_embed.description = "⏳ Updating tracking message..."
    await progress_msg.edit(embed=progress_embed)
    
    invite_manager.tracked_invites = new_invites
    invite_manager.save_data()
    await invite_manager.update_invite_message(channel)
    
    # Final update: Success!
    final_embed = EmbedBuilder.success(
        "✅ Invites Regenerated",
        f"Deleted **{deleted_count}** old invite(s) and generated **{len(new_invites)}** new one(s)!"
    )
    await progress_msg.edit(embed=final_embed)
    logging.info(f"[InviteManager] Invites regenerated by {ctx.author}")


@bot.hybrid_command(name="checkinvites", description="Display the status of all tracked invites")
async def checkinvites_cmd(ctx: commands.Context) -> None:
    global invite_manager
    
    if not invite_manager.tracked_invites:
        await ctx.send("No invites are being tracked. Use `/setupinvites` to set up.")
        return
    
    try:
        live_invites = await ctx.guild.invites()
        live_invite_data = {inv.code: inv for inv in live_invites}
    except Exception:
        live_invite_data = {}
    
    # Reuse the same richer embed builder the tracking message uses, so the
    # /checkinvites output looks consistent with the persistent panel.
    invite_rows: List[Dict[str, Any]] = []
    all_expired = True
    for code, data in invite_manager.tracked_invites.items():
        max_uses = data.get('max_uses', 0)
        current_uses = live_invite_data[code].uses if code in live_invite_data else data.get('uses', 0)
        remaining = max_uses - current_uses if max_uses > 0 else None
        is_marked_expired = data['status'] == 'expired' or data.get('expired', False)
        if is_marked_expired or (max_uses > 0 and remaining is not None and remaining <= 0):
            expired = True
        else:
            expired = False
            all_expired = False
        invite_rows.append({
            'code': code,
            'name': data.get('name', 'Invite'),
            'max_uses': max_uses,
            'uses': current_uses,
            'remaining': remaining,
            'expired': expired,
            'created_at': data.get('created_at'),
        })

    embed = invite_manager._build_invite_embed(invite_rows, all_expired)
    embed.set_footer(text=f"Requested by {ctx.author.display_name} • {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    # Attach the same view so /checkinvites also exposes the history button.
    view = InviteTrackingView(ctx.guild.id)
    await ctx.send(embed=embed, view=view)


@bot.hybrid_command(name="inviteinfo", description="Display detailed information about a specific invite")
@app_commands.describe(invite_code="The invite code to check")
@app_commands.autocomplete(invite_code=invite_code_autocomplete)
async def inviteinfo_cmd(ctx: commands.Context, invite_code: Optional[str] = None) -> None:
    global invite_manager
    
    if not invite_manager.tracked_invites:
        await ctx.send("No invites are being tracked.")
        return
    
    if not invite_code:
        await checkinvites_cmd(ctx)
        return
    
    invite_code = invite_code.replace("discord.gg/", "").replace("gg/", "").strip()
    
    if invite_code not in invite_manager.tracked_invites:
        await ctx.send(f"Invite `{invite_code}` is not being tracked.")
        return
    
    data = invite_manager.tracked_invites[invite_code]
    max_uses = data.get('max_uses', 0)
    
    try:
        live_invites = await ctx.guild.invites()
        current_uses = next((inv.uses for inv in live_invites if inv.code == invite_code), data.get('uses', 0))
    except Exception:
        current_uses = data.get('uses', 0)
    
    remaining = max_uses - current_uses
    is_expired = data['status'] == 'expired' or data.get('expired') or (remaining <= 0 and max_uses > 0)
    
    embed = discord.Embed(title=f"Invite: discord.gg/{invite_code}", color=discord.Color.green() if not is_expired else discord.Color.red())
    embed.add_field(name="Name", value=data['name'], inline=True)
    embed.add_field(name="Status", value="EXPIRED" if is_expired else "ACTIVE", inline=True)
    embed.add_field(name="Uses", value=f"{current_uses}/{max_uses} Used", inline=True)
    embed.add_field(name="Remaining", value=f"**{remaining}** uses left" if not is_expired else "None", inline=True)
    
    created_at = data.get('created_at', 'Unknown')
    embed.add_field(name="Created At", value=created_at[:10] if created_at != 'Unknown' else 'Unknown', inline=True)
    
    await ctx.send(embed=embed)


# --- VERIFICATION COMMANDS ---
@bot.hybrid_command()
@commands.has_permissions(manage_roles=True)
async def verifyuser(ctx: commands.Context, member: discord.Member) -> None:
    role = ctx.guild.get_role(config.roles.verified)
    if role not in member.roles:
        await member.add_roles(role)
        await ctx.send(embed=EmbedBuilder.success("User Verified", f"Welcome {member.mention} to GNG! You have been verified."))
        logging.info(f'User {member} was verified by {ctx.author}')
    else:
        await ctx.send(f"{member.mention} is already verified.")


@bot.hybrid_command()
@commands.has_permissions(manage_roles=True)
async def securitycheck(ctx: commands.Context, member: discord.Member) -> None:
    embed = discord.Embed(title=f"Security Check: {member.display_name}", color=discord.Color.blue())
    
    now = datetime.now(timezone.utc)
    created_at = member.created_at.replace(tzinfo=timezone.utc) if member.created_at.tzinfo is None else member.created_at
    account_age = (now - created_at).days
    
    age_risk = "HIGH" if account_age < 13 else "MEDIUM" if account_age < 30 else "LOW"
    embed.add_field(name="Account Age", value=f"{account_age} days ({age_risk} risk)", inline=True)
    
    join_age = (now - member.joined_at.replace(tzinfo=timezone.utc)).days if member.joined_at else "Unknown"
    embed.add_field(name="Time in Server", value=f"{join_age} days", inline=True)
    
    profile_flags: List[str] = []
    if not member.avatar:
        profile_flags.append("No profile picture")
    if len(member.display_name) < 3:
        profile_flags.append("Very short username")
    if member.display_name.isdigit():
        profile_flags.append("Username is all numbers")
    
    profile_risk = "HIGH" if len(profile_flags) >= 2 else "MEDIUM" if profile_flags else "LOW"
    embed.add_field(name="Profile Risk", value=f"{profile_risk}\n{', '.join(profile_flags) if profile_flags else 'No flags'}", inline=False)
    
    embed.set_thumbnail(url=member.avatar.url if member.avatar else member.default_avatar.url)
    embed.set_footer(text=f"User ID: {member.id}")
    
    await ctx.send(embed=embed)


@bot.hybrid_command()
@commands.cooldown(1, 3600, commands.BucketType.user)
async def verify(ctx: commands.Context) -> None:
    """
    Improved verification command — uses ImprovedVerificationSystem with
    modern embeds, button navigation, Roblox modal, and staff action view.
    The old text-based flow (_collect_verification_responses etc.) is replaced
    entirely; the legacy VerificationButtonsView is kept for any old submissions
    still open in the channel.
    """
    if not ows_get("enable_verification"):
        await ctx.send(embed=EmbedBuilder.warning(
            "🔐 Verification Disabled",
            "The verification system is currently disabled by the owner. Please try again later."
        ), delete_after=15)
        return

    if not ows_get("verification_cooldown"):
        verify.reset_cooldown(ctx)

    if ctx.author.id in process_manager._active_verifications:
        await ctx.send(
            embed=VerificationEmbedBuilder.create_base_embed(
                title="⏳ **VERIFICATION IN PROGRESS**",
                description="You already have an active verification session. Check your DMs!",
                color=VerificationEmbedBuilder.COLOR_WARNING
            )
        )
        return

    system = ImprovedVerificationSystem(bot, config, data_manager)
    await system.start_verification(ctx)


# --- REPORT SYSTEM ---
@bot.hybrid_command()
@commands.cooldown(1, 300, commands.BucketType.user)
async def report(ctx: commands.Context, member: Optional[discord.Member] = None) -> None:
    if not member:
        await ctx.send("Please mention the user you're reporting.")
        return
    
    if member == ctx.author:
        await ctx.send("You can't report yourself.")
        return
    
    if member.bot:
        await ctx.send("You can't report bots.")
        return
    
    embed = discord.Embed(title="Report System", description="React with the category number:", color=discord.Color.red())
    
    for emoji, cat in REPORT_CATEGORIES.items():
        embed.add_field(name=emoji, value=cat, inline=False)
    
    msg = await ctx.author.send(embed=embed)
    for emoji in REPORT_CATEGORIES.keys():
        await msg.add_reaction(emoji)
    
    try:
        reaction, _ = await bot.wait_for('reaction_add', check=lambda r, u: u == ctx.author and r.message.id == msg.id and str(r.emoji) in REPORT_CATEGORIES, timeout=60)
        category = REPORT_CATEGORIES[str(reaction.emoji)]
    except asyncio.TimeoutError:
        await ctx.author.send("Report timed out.")
        return
    
    await ctx.author.send(f"Please describe the {category} issue:")
    
    try:
        reason_msg = await bot.wait_for('message', check=lambda m: m.author == ctx.author and m.channel.type == discord.ChannelType.private, timeout=config.timing.report_timeout_seconds)
        
        if reason_msg.content.lower() == 'cancel':
            await ctx.author.send("Report cancelled.")
            return
        
        reason = reason_msg.content
    except asyncio.TimeoutError:
        await ctx.author.send("Report timed out.")
        return
    
    report_embed = discord.Embed(title=f"New {category} Report", color=discord.Color.red())
    report_embed.add_field(name="Reported User", value=f"{member.mention} (ID: {member.id})", inline=False)
    report_embed.add_field(name="Reporter", value=f"{ctx.author.mention} (ID: {ctx.author.id})", inline=False)
    report_embed.add_field(name="Reason", value=reason, inline=False)
    report_embed.timestamp = datetime.now(timezone.utc)
    
    reports_channel = bot.get_channel(config.channels.reports)
    if reports_channel:
        await reports_channel.send(embed=report_embed)
    
    await ctx.author.send("Your report has been submitted.")
    logging.info(f"New {category} report: {member} reported by {ctx.author}")


# --- MESSAGE TOGGLE ---
@bot.hybrid_command()
async def messageson(ctx: commands.Context) -> None:
    global messages_enabled
    
    if messages_enabled:
        await ctx.send("Message reporting is already enabled.")
        return
    
    messages_enabled = True
    if not send_report_message.is_running():
        send_report_message.start()
    await ctx.send(embed=EmbedBuilder.success("Messages Enabled", "Periodic report reminder messages have been enabled."))


@bot.hybrid_command()
async def messagesoff(ctx: commands.Context) -> None:
    global messages_enabled
    
    if not messages_enabled:
        await ctx.send("Message reporting is already disabled.")
        return
    
    messages_enabled = False
    send_report_message.stop()
    await ctx.send(embed=EmbedBuilder.info("Messages Disabled", "Periodic report reminder messages have been disabled."))


# --- POLL COMMAND ---
@bot.hybrid_command(name="poll", description="Create a poll with multiple options")
@commands.has_permissions(manage_messages=True)
@app_commands.describe(
    duration="Poll duration in seconds (10-600)",
    question="The poll question",
    options="Poll options separated by spaces (e.g. 'Red Blue Green') or pipes (e.g. 'Red|Blue|Green')",
)
async def poll(ctx: commands.Context, duration: int, question: str, *, options: str) -> None:
    """Create a live-updating poll.

    Converted from a prefix-only @bot.command() to @bot.hybrid_command() so it
    works with both `!poll` and `/poll`. The old `*options: str` variadic
    positional arg (which slash commands can't represent) is replaced with a
    single keyword-only `options: str` that gets split inside the function.
    Accepts either space-separated or pipe-separated options.
    """
    # Parse options: split on | if present, otherwise split on spaces.
    if "|" in options:
        option_list = [o.strip() for o in options.split("|") if o.strip()]
    else:
        option_list = [o.strip() for o in options.split() if o.strip()]
    options_tuple: Tuple[str, ...] = tuple(option_list)

    if len(options_tuple) < config.limits.min_poll_options or len(options_tuple) > config.limits.max_poll_options:
        await ctx.send(f"Please provide {config.limits.min_poll_options}-{config.limits.max_poll_options} options.")
        return

    if duration < config.limits.min_poll_duration or duration > config.limits.max_poll_duration:
        await ctx.send(f"Duration must be {config.limits.min_poll_duration}-{config.limits.max_poll_duration} seconds.")
        return

    embed = discord.Embed(title=f"{question}", description="React to vote!", color=discord.Color.blurple())

    for idx, option in enumerate(options_tuple):
        embed.add_field(name=f"{POLL_EMOJIS[idx]} {option}", value="(0 %)", inline=False)
    embed.set_footer(text=f"Poll ends in {duration}s")

    poll_msg = await ctx.send(embed=embed)

    for idx in range(len(options_tuple)):
        await poll_msg.add_reaction(POLL_EMOJIS[idx])

    end_time = time.time() + duration

    # Run the live-update loop as a detached background task so this command
    # returns immediately. Previously the command blocked the invoking task
    # for the entire poll duration, which scaled badly with multiple polls.
    asyncio.create_task(_run_poll(poll_msg, question, options_tuple, end_time))


async def _run_poll(poll_msg: discord.Message, question: str, options: Tuple[str, ...], end_time: float) -> None:
    """
    Background task that live-updates a poll until it ends, then shows results.

    Runs detached from the command so multiple concurrent polls don't pin a
    command task each. Exits cleanly if the poll message is deleted or becomes
    inaccessible.
    """
    results: Dict[int, int] = {}
    total_votes = 0
    try:
        while time.time() < end_time:
            await asyncio.sleep(15)
            try:
                poll_msg, results, total_votes = await _update_poll(poll_msg, question, options, end_time)
            except discord.NotFound:
                logging.info(f"[Poll] Poll message {poll_msg.id} was deleted; aborting poll loop.")
                return
            except discord.Forbidden:
                logging.warning(f"[Poll] Lost permission to edit poll message {poll_msg.id}; aborting.")
                return
        await _show_poll_results(poll_msg, question, options, results, total_votes)
    except asyncio.CancelledError:
        # Bot is shutting down; let the cancellation propagate.
        raise
    except Exception as e:
        logging.error(f"[Poll] Poll loop error for message {poll_msg.id}: {e}")


async def _update_poll(poll_msg: discord.Message, question: str, options: Tuple[str, ...], end_time: float) -> Tuple[discord.Message, Dict[int, int], int]:
    poll_msg = await poll_msg.channel.fetch_message(poll_msg.id)
    results: Dict[int, int] = {}
    total_votes = 0
    
    for idx, emoji in enumerate(POLL_EMOJIS[:len(options)]):
        for reaction in poll_msg.reactions:
            if str(reaction.emoji) == emoji:
                votes = reaction.count - 1
                results[idx] = votes
                total_votes += votes
    
    embed = discord.Embed(title=f"{question}", description="React to vote!", color=discord.Color.blurple())
    
    for idx, option in enumerate(options):
        votes = results.get(idx, 0)
        pct = (votes / total_votes * 100) if total_votes > 0 else 0
        bar = '█' * int(pct/10) + '░' * (10-int(pct/10))
        embed.add_field(name=f"{POLL_EMOJIS[idx]} {option}", value=f"{bar} {votes} ({pct:.1f}%)", inline=False)
    
    embed.set_footer(text=f"{int(end_time - time.time())}s remaining")
    await poll_msg.edit(embed=embed)
    
    return poll_msg, results, total_votes


async def _show_poll_results(poll_msg: discord.Message, question: str, options: Tuple[str, ...], results: Dict[int, int], total_votes: int) -> None:
    result_embed = discord.Embed(title=f"Poll Results: {question}", color=discord.Color.green())
    
    for idx, option in enumerate(options):
        votes = results.get(idx, 0)
        pct = (votes / total_votes * 100) if total_votes > 0 else 0
        bar = '█' * int(pct/10) + '░' * (10-int(pct/10))
        result_embed.add_field(name=f"{POLL_EMOJIS[idx]} {option}", value=f"{bar} {votes} ({pct:.1f}%)", inline=False)
    
    await poll_msg.edit(embed=result_embed)
    await poll_msg.clear_reactions()


# --- SHUTDOWN & STATUS COMMANDS ---
@bot.hybrid_command()
@commands.has_permissions(administrator=True)
async def shutdown(ctx: commands.Context) -> None:
    if process_manager.is_busy():
        await ctx.send("Cannot shutdown: Users are currently in verification process!")
        return

    await ctx.send("Shutting down bot...")
    logging.info(f"Bot shutdown initiated by {ctx.author}")
    save_all_data()
    reset_temporary_data()
    data_manager.close()
    process_manager.clear_lock_file()
    await bot.close()


@bot.hybrid_command()
@commands.has_permissions(administrator=True)
async def botstatus(ctx: commands.Context) -> None:
    embed = discord.Embed(title="Bot Status", color=discord.Color.blue())
    embed.add_field(name="Uptime", value=get_uptime(), inline=True)
    embed.add_field(name="Latency", value=f"{round(bot.latency * 1000)}ms", inline=True)
    embed.add_field(name="Active Verifications", value=str(len(process_manager._active_verifications)), inline=True)
    
    if process_manager.is_busy():
        embed.color = discord.Color.orange()
        embed.add_field(name="Status", value="⚠️ Bot is busy! Avoid restarting.", inline=False)
    else:
        embed.color = discord.Color.green()
        embed.add_field(name="Status", value="✅ Bot is idle. Safe to restart.", inline=False)
    
    await ctx.send(embed=embed)


# --- AUDIT LOG ---
@bot.hybrid_command()
@commands.has_permissions(view_audit_log=True)
async def auditlog(ctx: commands.Context, limit: int = 10) -> None:
    try:
        entries: List[str] = []
        
        async for entry in ctx.guild.audit_logs(limit=limit):
            entries.append(f"**{entry.created_at.strftime('%Y-%m-%d %H:%M:%S')}** | {str(entry.action).split('.')[-1]} | User: {entry.user} | Target: {entry.target} | Reason: {entry.reason or 'None'}")
        
        if not entries:
            await ctx.send("No audit log entries found.")
            return
        
        for chunk in [entries[i:i+10] for i in range(0, len(entries), 10)]:
            await ctx.send("\n".join(chunk))
            
    except discord.Forbidden:
        await ctx.send("I don't have permission to view audit logs.")


class PaginatedHelpView(View):
    """Paginated view for the `!cmds` / `/cmds` help command.

    Shows one page at a time with Previous / Next / Close buttons.
    Only the user who invoked the command can navigate.
    """

    def __init__(self, user_id: int, pages: List[discord.Embed], timeout: float = 300, initial_page: int = 0):
        super().__init__(timeout=timeout)
        self.user_id = user_id
        self.pages = pages
        
        # Clamp the initial page to make sure it doesn't exceed the number of pages we have
        max_page = len(pages) - 1 if pages else 0
        self.current_page = min(initial_page, max_page) if max_page > 0 else 0
        
        self._update_buttons()

    def _update_buttons(self) -> None:
        self.prev_button.disabled = self.current_page == 0
        self.next_button.disabled = self.current_page >= len(self.pages) - 1

    def current_embed(self) -> discord.Embed:
        embed = self.pages[self.current_page]
        # Update the footer to reflect the current page number.
        embed.set_footer(text=f"Page {self.current_page + 1}/{len(self.pages)} • {brand_text('[GANG ABBR]')} Commands")
        return embed

    @discord.ui.button(label="⬅ Previous", style=discord.ButtonStyle.secondary, custom_id="help_prev_page")
    async def prev_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your help menu!", ephemeral=True)
            return
        if self.current_page > 0:
            self.current_page -= 1
            self._update_buttons()
            await interaction.response.edit_message(embed=self.current_embed(), view=self)
        else:
            await interaction.response.defer()

    @discord.ui.button(label="Next ➡", style=discord.ButtonStyle.secondary, custom_id="help_next_page")
    async def next_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your help menu!", ephemeral=True)
            return
        if self.current_page < len(self.pages) - 1:
            self.current_page += 1
            self._update_buttons()
            await interaction.response.edit_message(embed=self.current_embed(), view=self)
        else:
            await interaction.response.defer()

    @discord.ui.button(label="✖ Close", style=discord.ButtonStyle.danger, custom_id="help_close")
    async def close_button(self, interaction: discord.Interaction, button: discord.ui.Button) -> None:
        if interaction.user.id != self.user_id:
            await interaction.response.send_message("This isn't your help menu!", ephemeral=True)
            return
        self.stop()
        try:
            await interaction.message.delete()
        except discord.HTTPException:
            await interaction.response.defer()

    async def on_timeout(self) -> None:
        """Disable all buttons when the view times out."""
        for child in self.children:
            child.disabled = True


def build_help_pages() -> List[discord.Embed]:
    """Build the paginated help embeds for the `!cmds` command.

    The help content is organized into category sections. Sections are
    grouped into pages where each page's total text stays under ~1800 chars
    (well within Discord's 4096-char embed description limit and the 2000-char
    content limit). Categories are never split across pages.
    """
    # Each section: (category_title, list_of_command_lines)
    sections: List[Tuple[str, List[str]]] = [
        ("🛡️ Moderation", [
            "`!kick @user1 @user2 [reason]` - Kick one or multiple users",
            "`!ban @user1 @user2 [reason]` - Ban one or multiple users",
            "`!banid <user_id> [reason]` - Ban user by ID",
            "`!softban @user [reason]` - Softban a user",
            "`!mute @user [reason]` - Mute a user",
            "`!unmute @user` - Unmute a user",
            "`!tempmute @user [seconds] [reason]` - Temporarily mute",
            "`!purge [amount] [-del]` - Delete messages (leave empty for all). `-del` also removes the command message and skips the confirmation reply",
            "`!lock` - Lock current channel",
            "`!unlock` - Unlock current channel",
            "`!slowmode [seconds]` - Set slowmode",
        ]),
        ("🚫 Blacklist System", [
            "`/blacklist <keyword>` - Add keyword to blacklist",
            "`/unblacklist <keyword>` - Remove keyword from blacklist",
            "`/blacklistlist` - Show all blacklisted keywords",
            "`/blacklistscan` - Scan all members for blacklisted keywords",
            "`/checkprofile [@user]` - Check a user's profile",
        ]),
        ("⚠️ Warnings", [
            "`/warn @user <reason>` - Warn a member",
            "`/warnings [@user]` - View warnings",
            "`/clearwarnings @user` - Clear warnings",
        ]),
        ("🎫 Tickets", [
            "`/panel` - Create a ticket panel (interactive)",
            "`/panels` - List all ticket panels",
            "`/deletepanel <panel_id>` - Delete a panel",
            "`/claim` - Claim the current ticket (atomic — no more race)",
            "`/unclaim` - Release your claim",
            "`/close [reason]` - Close the current ticket (transcript-first ordering)",
            "`/transcript` - Generate a transcript (full history — no 500 cap)",
            "`/tickets` - View all open tickets",
            "`/ticketsettings` - Configure ticket system",
            "`/ticketblacklist @user [reason]` - Block user from tickets",
            "`/ticketunblacklist @user` - Remove user from blacklist",
            "`/add @user` - Add a user to the current ticket",
            "`/remove @user` - Remove a user from the current ticket",
            "`/rename <name>` - Rename the ticket channel",
            "`/move <panel_id>` - Move ticket to a different panel/category",
            "`/note <text>` - Add a private staff note (no longer leaks to ticket creator)",
            "`/notes` - View all staff notes for this ticket",
            "`/priority <level>` - Set ticket priority (low/normal/high/urgent)",
            "`/reopen <ticket_id>` - Reopen a closed ticket",
            "`/ticketstats` - View ticket statistics for this server",
            "`/dbcleanup` - Scan and remove stale/invalid database entries",
            "",
            "_(Ticket buttons + panels survive bot restarts; claim/close are race-free)_",
        ]),
        ("🎉 Giveaways", [
            "`/giveaway <hours> <winners> <prize>` - Create giveaway",
            "`/endgiveaway <id>` - End a giveaway early",
        ]),
        ("📊 Leveling", [
            "`/level [@user]` - View your level",
            "`/leaderboard` - View server leaderboard",
        ]),
        ("📋 Rules Management", [
            "`/updategangrules` - Fetch and cache gang rules",
            "`/updateserverrules` - Fetch and cache server rules",
            "`/setgangrules <text>` - Manually set gang rules",
            "`/setserverrules <text>` - Manually set server rules",
            "`/viewcachedrules` - View cached rules status",
        ]),
        ("👥 Roles", [
            "`!addrole @user [role_name]` - Add role to user",
            "`!removerole @user @role` - Remove role from user",
            "`!roleall @role` - Give role to all members",
            "`!getallroles` or `/getallroles` - List all roles & IDs with a **Copy List** button (owner only, auto-updates on role changes, survives restarts)",
        ]),
        ("🔐 Verification", [
            "`!verify` - Start verification",
            "`!verifyuser @user` - Manually verify user",
            "`!securitycheck @user` - Check user security",
        ]),
        ("📨 Invites", [
            "`/setupinvites` - Set up invite tracking (posts the panel + View Previous Invites button)",
            "`/checkinvites` - View invite status (rich embed + history button)",
            "`/inviteinfo [code]` - View specific invite",
            "`/regenerateinvites` - Regenerate all invites (archives the old batch first)",
            "",
            "_(Old invite batches are archived — click 'View Previous Invites' on the panel to browse them, paginated)_",
        ]),
        ("ℹ️ Information", [
            "`!userinfo @user` - User information",
            "`!serverinfo` - Server information",
            "`!membercount` - Member count",
            "`!ping` - Bot latency",
            "`!uptime` - Bot uptime",
            "`!botstatus` - Check active processes",
        ]),
        ("⭐ Premium Features (FREE Carl-bot / Dyno clones)", [
            "`/rr add #channel <msg_id> <emoji> <@role> [mode]` - Add reaction role",
            "`/rr list` - List all reaction roles (up to 250)",
            "`/rr remove <mapping_id>` - Remove one mapping",
            "`/rr clear <msg_id>` - Clear all mappings on a message",
            "`/rr creator <msg_id>` - Quick modal creator",
            "`/stickyrole enable` - Turn ON sticky roles (re-apply on rejoin)",
            "`/stickyrole add @role` - Mark a role as sticky-eligible",
            "`/stickyrole list` / `/stickyrole status [@member]`",
            "`/msglog enable #channel` - Log edits + deletes with content",
            "`/msglog edits on|off` / `/msglog deletes on|off`",
            "`/msglog ignore #channel` / `/msglog status`",
            "`/botbranding footer <text>` - Custom embed footer",
            "`/botbranding color #RRGGBB` / `thumbnail <url>` / `image <url>`",
            "`/botbranding banner <url>` / `avatar <url>` (owner)",
            "`/botbranding view` / `/botbranding clear`",
        ]),
        ("📦 Other", [
            "`!report @user` - Report a user",
            "`!poll [duration] [question] [options...]` or `/poll` - Create a poll (accepts space- or pipe-separated options)",
            "`!auditlog [limit]` - View audit logs",
            "`!shutdown` - Gracefully shutdown bot",
            "`!tutorial` - Re-send the setup tutorial (owner only)",
            "`!sync [guild_id]` - Sync slash commands (owner only)",
            "",
            "**💡 Multi-Command Chaining:**",
            "Run multiple commands in one message separated by commas!",
            "Example: `!cmds, !setupinvites, !regenerateinvites`",
            "*(Staff automatically get a prompt to clean up the command message — except when using `!purge ... -del`, which deletes itself)*",
        ]),
    ]

    # Group sections into pages. Each page is a list of (title, lines).
    # We keep each page under MAX_PAGE_CHARS so the embed description stays
    # well within Discord's limits.
    MAX_PAGE_CHARS = 1800
    pages_sections: List[List[Tuple[str, List[str]]]] = []
    current: List[Tuple[str, List[str]]] = []
    current_len = 0

    for title, lines in sections:
        # Render this section to estimate its length.
        rendered = f"**{title}**\n" + "\n".join(lines)
        section_len = len(rendered)

        # If this section alone exceeds the limit, it gets its own page.
        if section_len > MAX_PAGE_CHARS:
            # Flush the current page first.
            if current:
                pages_sections.append(current)
                current = []
                current_len = 0
            pages_sections.append([(title, lines)])
            continue

        # If adding this section would overflow, flush and start a new page.
        if current_len + section_len + 4 > MAX_PAGE_CHARS and current:
            pages_sections.append(current)
            current = []
            current_len = 0

        current.append((title, lines))
        current_len += section_len + 4  # +4 for the blank line between sections

    if current:
        pages_sections.append(current)

    # Build one embed per page.
    embeds: List[discord.Embed] = []
    colors = [
        discord.Color.blurple(),
        discord.Color.green(),
        discord.Color.orange(),
        discord.Color.red(),
        discord.Color.purple(),
        discord.Color.teal(),
    ]
    for idx, page_sections in enumerate(pages_sections):
        description_parts: List[str] = []
        for title, lines in page_sections:
            description_parts.append(f"**{title}**")
            description_parts.extend(lines)
            description_parts.append("")  # blank line between sections
        description = "\n".join(description_parts).strip()

        embed = discord.Embed(
            title=f"📜 {brand_text('[GANG ABBR]')} — Available Commands",
            description=description,
            color=colors[idx % len(colors)],
            timestamp=datetime.now(timezone.utc),
        )
        # Footer is set dynamically by the view, but set a default here too.
        embed.set_footer(text=f"Page {idx + 1}/{len(pages_sections)} • {brand_text('[GANG ABBR]')} Commands")
        embeds.append(embed)

    return embeds


# =============================================================================
# PREMIUM FEATURES (FREE CLONES OF CARL-BOT / DYNO PREMIUM)
#   1. Reaction Roles  (Carl-bot signature — up to 250 per guild)
#   2. Sticky Roles    (Dyno premium — re-apply roles on rejoin)
#   3. Full Message Logging (Dyno premium — edit/delete content)
#   4. Custom Bot Branding  (avatar / banner / footer — premium feel)
# =============================================================================

# --- Helpers ---------------------------------------------------------------

def _emoji_key(emoji) -> str:
    """Normalise a reaction emoji (str | PartialEmoji | Emoji) into a stable
    string key that matches what we store in the DB."""
    if isinstance(emoji, (discord.Emoji, discord.PartialEmoji)):
        if emoji.id is None:
            return emoji.name  # unicode emoji
        if emoji.animated:
            return f"<a:{emoji.name}:{emoji.id}>"
        return f"<:{emoji.name}:{emoji.id}>"
    return str(emoji)


def _hex_to_int(color_hex: str) -> Optional[int]:
    """Parse '#RRGGBB' / 'RRGGBB' / '0xRRGGBB' into an int. Returns None on bad input."""
    if not color_hex:
        return None
    raw = color_hex.strip().lstrip('#')
    if raw.lower().startswith('0x'):
        raw = raw[2:]
    if not re.fullmatch(r'[0-9a-fA-F]{6}', raw):
        return None
    return int(raw, 16)


async def _fetch_image_bytes(url: str) -> Optional[bytes]:
    """Download image bytes from a URL using aiohttp (bundled with discord.py)."""
    try:
        import aiohttp  # discord.py depends on aiohttp, so always available
    except ImportError:
        logging.error("[Branding] aiohttp not available to fetch avatar image")
        return None
    try:
        timeout = aiohttp.ClientTimeout(total=20)
        async with aiohttp.ClientSession(timeout=timeout) as session:
            async with session.get(url) as resp:
                if resp.status != 200:
                    logging.warning(f"[Branding] avatar fetch HTTP {resp.status}")
                    return None
                data = await resp.read()
                if not data:
                    return None
                # Discord avatar limit is 10 MB; bail out early if absurdly large.
                if len(data) > 10 * 1024 * 1024:
                    logging.warning("[Branding] avatar image too large (>10MB)")
                    return None
                return data
    except Exception as exc:
        logging.warning(f"[Branding] avatar fetch failed: {exc}")
        return None


# =========================================================================
# 1) REACTION ROLES — Carl-bot signature feature (up to 250 per guild)
# =========================================================================
class ReactionRoleSystem:
    """
    Modes:
      normal  — toggle (add on react, remove on unreact)   [default]
      verify  — one-way (add on react, never auto-remove)
      reverse — remove role on react, add on unreact
      unique  — only one role per message; reacting removes your other
                roles on the same message and gives the new one
    """

    VALID_MODES = {"normal", "verify", "reverse", "unique"}

    @staticmethod
    async def add_mapping(
        guild: discord.Guild,
        channel: discord.TextChannel,
        message_id: int,
        emoji_raw,
        role: discord.Role,
        mode: str = "normal",
        description: str = "",
        actor: Optional[discord.Member] = None,
    ) -> Tuple[bool, str]:
        if mode not in ReactionRoleSystem.VALID_MODES:
            return False, f"Invalid mode. Valid: {', '.join(sorted(ReactionRoleSystem.VALID_MODES))}"

        # Hierarchy check — the bot's top role must be above the target role.
        if guild.me.top_role <= role:
            return False, f"My highest role ({guild.me.top_role.mention}) must be above {role.mention}."

        if role.managed:
            return False, f"{role.mention} is a managed/integration role and can't be assigned."

        # Enforce the 250-per-guild premium cap (Carl-bot's signature limit).
        count = data_manager.count_reaction_roles(guild.id)
        if count >= DataManager.MAX_REACTION_ROLES_PER_GUILD:
            return False, (
                f"Reaction role cap reached: **{count}/{DataManager.MAX_REACTION_ROLES_PER_GUILD}**. "
                "Remove an existing mapping before adding more."
            )

        # Fetch the message so we can (a) validate it exists and (b) add the
        # reaction emoji so the button is visible to users.
        try:
            message = await channel.fetch_message(message_id)
        except discord.NotFound:
            return False, "That message no longer exists."
        except discord.Forbidden:
            return False, "I can't read that channel."
        except discord.HTTPException as exc:
            return False, f"Could not fetch message: {exc}"

        emoji_str = _emoji_key(emoji_raw)

        # Duplicate check (same message + emoji already mapped).
        existing = data_manager.find_reaction_role(message_id, emoji_str)
        if existing:
            return False, f"That emoji is already mapped to <@&{existing['role_id']}> on this message."

        mapping_id = str(_uuid.uuid4())
        data_manager.save_reaction_role({
            'mapping_id': mapping_id,
            'guild_id': guild.id,
            'channel_id': channel.id,
            'message_id': message_id,
            'emoji': emoji_str,
            'role_id': role.id,
            'mode': mode,
            'description': description,
            'created_by': actor.id if actor else None,
            'created_at': datetime.now(timezone.utc).isoformat(),
            'is_active': True,
        })

        # Add the bot's reaction so users see the "button".
        try:
            await message.add_reaction(emoji_raw if not isinstance(emoji_raw, str) else emoji_str)
        except (discord.Forbidden, discord.HTTPException) as exc:
            logging.warning(f"[RR] Could not add reaction {emoji_str} to msg {message_id}: {exc}")

        logging.info(
            f"[RR] Added mapping {emoji_str} -> {role.name} (mode={mode}) in {guild.id} "
            f"by {actor}"
        )
        return True, mapping_id

    @staticmethod
    async def handle_reaction(
        payload: "discord.RawReactionActionEvent", added: bool
    ) -> None:
        """Core reaction-role logic. `added` = True for add, False for remove."""
        if payload.guild_id is None or payload.member is None:
            return
        if payload.member.bot:
            return

        mapping = data_manager.find_reaction_role(payload.message_id, _emoji_key(payload.emoji))
        if not mapping:
            return

        guild = bot.get_guild(payload.guild_id)
        if guild is None:
            return
        role = guild.get_role(mapping['role_id'])
        if role is None:
            return

        # Hierarchy / permission safety.
        if guild.me.top_role <= role or not guild.me.guild_permissions.manage_roles:
            return

        mode = mapping.get('mode', 'normal')
        member = payload.member

        try:
            if mode == 'verify':
                # One-way: only add, never remove.
                if added:
                    await member.add_roles(role, reason="Reaction role (verify)")
            elif mode == 'reverse':
                if added:
                    await member.remove_roles(role, reason="Reaction role (reverse)")
                else:
                    await member.add_roles(role, reason="Reaction role (reverse) restore")
            elif mode == 'unique':
                if added:
                    # Remove all OTHER roles mapped on the same message first.
                    others = data_manager.load_reaction_roles_by_message(payload.message_id)
                    other_roles = [
                        guild.get_role(m['role_id']) for m in others
                        if m['role_id'] != mapping['role_id'] and m.get('mode') == 'unique'
                    ]
                    other_roles = [r for r in other_roles if r is not None and guild.me.top_role > r]
                    if other_roles:
                        await member.remove_roles(*other_roles, reason="Reaction role (unique) swap")
                    await member.add_roles(role, reason="Reaction role (unique)")
            else:  # normal
                if added:
                    await member.add_roles(role, reason="Reaction role")
                else:
                    await member.remove_roles(role, reason="Reaction role removal")
        except discord.Forbidden:
            logging.warning(f"[RR] Missing permissions to manage {role.name} in {guild.id}")
        except discord.HTTPException as exc:
            logging.warning(f"[RR] HTTP error managing roles: {exc}")


class ReactionRoleCreatorModal(Modal, title="➕ Add Reaction Role Mapping"):
    """Quick modal-based creator: emoji + role id + mode."""

    emoji_input = TextInput(
        label="Emoji (paste an emoji, or :name:id for custom)",
        placeholder="👍  or  :star:123456789",
        required=True,
        max_length=80,
    )
    role_input = TextInput(
        label="Role ID (right-click role → Copy ID)",
        placeholder="123456789012345678",
        required=True,
        max_length=20,
    )
    mode_input = TextInput(
        label="Mode (normal / verify / reverse / unique)",
        placeholder="normal",
        required=False,
        max_length=10,
    )

    def __init__(self, channel: discord.TextChannel, message_id: int):
        super().__init__()
        self.channel = channel
        self.message_id = message_id

    async def on_submit(self, interaction: discord.Interaction) -> None:
        emoji_raw = self.emoji_input.value.strip()
        mode = (self.mode_input.value.strip() or "normal").lower()

        # Parse role id.
        try:
            role_id = int(self.role_input.value.strip())
        except ValueError:
            await interaction.response.send_message(
                embed=EmbedBuilder.error("Invalid Role ID", "Role ID must be a number."),
                ephemeral=True,
            )
            return

        guild = interaction.guild
        role = guild.get_role(role_id)
        if role is None:
            await interaction.response.send_message(
                embed=EmbedBuilder.error("Role Not Found", f"No role with ID `{role_id}` in this server."),
                ephemeral=True,
            )
            return

        ok, msg = await ReactionRoleSystem.add_mapping(
            guild, self.channel, self.message_id, emoji_raw, role,
            mode=mode, actor=interaction.user,
        )
        if ok:
            await interaction.response.send_message(
                embed=EmbedBuilder.branded(EmbedBuilder.success(
                    "Reaction Role Added",
                    f"{emoji_raw} → {role.mention}\nMode: **{mode}**\nMapping ID: `{msg}`",
                ), guild.id),
                ephemeral=True,
            )
        else:
            await interaction.response.send_message(
                embed=EmbedBuilder.error("Could Not Add Reaction Role", msg),
                ephemeral=True,
            )


@bot.hybrid_group(name="reactionrole", aliases=["rr"], description="Reaction Roles (Carl-bot style, up to 250)")
@app_commands.default_permissions(manage_roles=True)
async def rr_group(ctx: commands.Context) -> None:
    if ctx.invoked_subcommand is None:
        await ctx.send(embed=EmbedBuilder.info(
            "Reaction Roles",
            "Subcommands: `add`, `remove`, `list`, `clear`, `creator`.\n"
            "Example: `/rr add #channel <message_id> <emoji> <@role> [mode]`",
        ), ephemeral=True)


@rr_group.command(name="add", description="Bind an emoji on a message to a role")
@app_commands.describe(
    channel="The channel containing the message",
    message_id="The message ID to attach the reaction role to",
    emoji="Emoji (paste any emoji, or :name:id for a custom one)",
    role="The role to grant/toggle",
    mode="normal (toggle) / verify (one-way) / reverse / unique",
    description="Optional note about this mapping",
)
@commands.has_permissions(manage_roles=True)
async def rr_add(
    ctx: commands.Context,
    channel: discord.TextChannel,
    message_id: str,
    emoji: str,
    role: discord.Role,
    mode: str = "normal",
    description: str = "",
) -> None:
    try:
        msg_id_int = int(message_id)
    except ValueError:
        await ctx.send(embed=EmbedBuilder.error("Bad Message ID", "Message ID must be numeric."), ephemeral=True)
        return

    ok, msg = await ReactionRoleSystem.add_mapping(
        ctx.guild, channel, msg_id_int, emoji, role,
        mode=mode, description=description, actor=ctx.author,
    )
    if ok:
        await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
            "Reaction Role Added ✅",
            f"{emoji} → {role.mention}\nMode: **{mode}**\nMessage: {channel.mention} (`{msg_id_int}`)\nMapping ID: `{msg}`",
        ), ctx.guild.id))
    else:
        await ctx.send(embed=EmbedBuilder.error("Could Not Add Reaction Role", msg), ephemeral=True)


@rr_group.command(name="remove", description="Remove a reaction-role mapping by ID")
@app_commands.describe(mapping_id="The mapping ID shown by /rr list")
@commands.has_permissions(manage_roles=True)
async def rr_remove(ctx: commands.Context, mapping_id: str) -> None:
    deleted = data_manager.delete_reaction_role(mapping_id.strip())
    if deleted:
        await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
            "Reaction Role Removed", f"Mapping `{mapping_id}` was deleted."
        ), ctx.guild.id))
    else:
        await ctx.send(embed=EmbedBuilder.error("Not Found", f"No mapping with ID `{mapping_id}`."), ephemeral=True)


@rr_group.command(name="list", description="List all reaction-role mappings in this server")
@commands.has_permissions(manage_roles=True)
async def rr_list(ctx: commands.Context) -> None:
    mappings = data_manager.load_reaction_roles_by_guild(ctx.guild.id)
    count = len(mappings)
    cap = DataManager.MAX_REACTION_ROLES_PER_GUILD
    if not mappings:
        await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.info(
            "Reaction Roles",
            f"No reaction roles configured yet.\nCapacity: **0/{cap}** (Carl-bot limit, free).",
        ), ctx.guild.id), ephemeral=True)
        return

    embed = discord.Embed(
        title="🎚️ Reaction Roles",
        description=f"**{count}/{cap}** mappings in **{ctx.guild.name}**",
        color=discord.Color.blurple(),
        timestamp=datetime.now(timezone.utc),
    )
    # Group by message for readability.
    by_msg: Dict[int, List[Dict]] = {}
    for m in mappings:
        by_msg.setdefault(m['message_id'], []).append(m)

    lines: List[str] = []
    for msg_id, group in by_msg.items():
        chan = ctx.guild.get_channel(group[0]['channel_id'])
        chan_name = chan.mention if chan else f"~deleted#{group[0]['channel_id']}"
        lines.append(f"**[{chan_name}]** msg `{msg_id}` ({len(group)} role{'s' if len(group)!=1 else ''})")
        for m in group[:20]:
            role = ctx.guild.get_role(m['role_id'])
            role_name = role.mention if role else f"~deleted:{m['role_id']}"
            lines.append(f"  • {m['emoji']} → {role_name} · `{m['mode']}` · `{m['mapping_id'][:8]}`")
        if len(group) > 20:
            lines.append(f"  • …and {len(group)-20} more")
        lines.append("")
        if len("\n".join(lines)) > 3800:
            lines.append("_(list truncated — use /rr remove with a mapping id to clean up)_")
            break

    embed.add_field(name="Mappings", value="\n".join(lines), inline=False)
    await ctx.send(embed=EmbedBuilder.branded(embed, ctx.guild.id))


@rr_group.command(name="clear", description="Remove ALL reaction-role mappings on a message")
@app_commands.describe(message_id="The message ID whose mappings should be cleared")
@commands.has_permissions(manage_roles=True)
async def rr_clear(ctx: commands.Context, message_id: str) -> None:
    try:
        msg_id_int = int(message_id)
    except ValueError:
        await ctx.send(embed=EmbedBuilder.error("Bad Message ID", "Message ID must be numeric."), ephemeral=True)
        return
    removed = data_manager.delete_reaction_roles_for_message(msg_id_int)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Reaction Roles Cleared", f"Removed **{removed}** mapping(s) from message `{msg_id_int}`."
    ), ctx.guild.id))


@rr_group.command(name="creator", description="Open a quick modal to add a reaction role to a message")
@app_commands.describe(message_id="The message ID to attach the reaction role to",
                       channel="The channel containing the message (defaults to this one)")
@commands.has_permissions(manage_roles=True)
async def rr_creator(
    ctx: commands.Context,
    message_id: str,
    channel: Optional[discord.TextChannel] = None,
) -> None:
    try:
        msg_id_int = int(message_id)
    except ValueError:
        await ctx.send(embed=EmbedBuilder.error("Bad Message ID", "Message ID must be numeric."), ephemeral=True)
        return
    target_channel = channel or ctx.channel
    # Verify the message exists before showing the modal.
    try:
        await target_channel.fetch_message(msg_id_int)
    except Exception as exc:
        await ctx.send(embed=EmbedBuilder.error("Message Not Found", f"Could not fetch that message: {exc}"), ephemeral=True)
        return
    await ctx.send_modal(ReactionRoleCreatorModal(target_channel, msg_id_int))


# =========================================================================
# 2) STICKY ROLES — Dyno premium (re-apply roles on rejoin)
# =========================================================================
class StickyRoleSystem:
    @staticmethod
    def get_eligible_roles(guild_id: int) -> List[int]:
        cfg = data_manager.get_sticky_role_config(guild_id)
        try:
            return [int(r) for r in json.loads(cfg.get('eligible_role_ids', '[]'))]
        except Exception:
            return []

    @staticmethod
    def set_eligible_roles(guild_id: int, role_ids: List[int]) -> None:
        cfg = data_manager.get_sticky_role_config(guild_id)
        cfg['eligible_role_ids'] = json.dumps(role_ids)
        cfg['enabled'] = 1
        cfg['updated_at'] = datetime.now(timezone.utc).isoformat()
        data_manager.save_sticky_role_config(cfg)

    @staticmethod
    def set_enabled(guild_id: int, enabled: bool) -> None:
        cfg = data_manager.get_sticky_role_config(guild_id)
        cfg['enabled'] = 1 if enabled else 0
        cfg['updated_at'] = datetime.now(timezone.utc).isoformat()
        data_manager.save_sticky_role_config(cfg)

    @staticmethod
    def is_enabled(guild_id: int) -> bool:
        return bool(data_manager.get_sticky_role_config(guild_id).get('enabled', 0))

    @staticmethod
    def capture_member_roles(member: discord.Member) -> None:
        """Save the member's eligible roles so they can be restored on rejoin."""
        if not StickyRoleSystem.is_enabled(member.guild.id):
            return
        eligible = set(StickyRoleSystem.get_eligible_roles(member.guild.id))
        if not eligible:
            # No eligible list configured → save all non-managed, non-everyone roles.
            saved = [
                r.id for r in member.roles
                if not r.managed and r.id != member.guild.default_role.id
            ]
        else:
            saved = [r.id for r in member.roles if r.id in eligible]
        # Always save at least an empty list so we know we've seen this member.
        data_manager.save_sticky_roles(member.guild.id, member.id, saved)
        logging.info(
            f"[Sticky] Saved {len(saved)} role(s) for {member} leaving {member.guild.id}"
        )

    @staticmethod
    async def restore_member_roles(member: discord.Member) -> int:
        """Re-apply previously saved roles. Returns count of roles restored."""
        if not StickyRoleSystem.is_enabled(member.guild.id):
            return 0
        saved = data_manager.load_sticky_roles(member.guild.id, member.id)
        if not saved:
            return 0

        guild = member.guild
        eligible = set(StickyRoleSystem.get_eligible_roles(guild.id))
        to_add: List[discord.Role] = []
        for rid in saved:
            role = guild.get_role(rid)
            if role is None:
                continue
            if role.managed:
                continue
            if eligible and rid not in eligible:
                continue
            # Hierarchy safety.
            if guild.me.top_role <= role:
                continue
            to_add.append(role)

        if not to_add:
            return 0
        try:
            await member.add_roles(*to_add, reason="Sticky role restore on rejoin")
            logging.info(f"[Sticky] Restored {len(to_add)} role(s) to {member} rejoining {guild.id}")
            return len(to_add)
        except (discord.Forbidden, discord.HTTPException) as exc:
            logging.warning(f"[Sticky] Could not restore roles to {member}: {exc}")
            return 0


@bot.hybrid_group(name="stickyrole", aliases=["stickyroles", "sr"], description="Sticky Roles (Dyno premium — re-apply on rejoin)")
@app_commands.default_permissions(manage_roles=True)
async def sticky_group(ctx: commands.Context) -> None:
    if ctx.invoked_subcommand is None:
        await ctx.send(embed=EmbedBuilder.info(
            "Sticky Roles",
            "Subcommands: `enable`, `disable`, `add`, `remove`, `list`, `status`.",
        ), ephemeral=True)


@sticky_group.command(name="enable", description="Turn ON sticky roles for this server")
@commands.has_permissions(manage_roles=True)
async def sticky_enable(ctx: commands.Context) -> None:
    StickyRoleSystem.set_enabled(ctx.guild.id, True)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Sticky Roles Enabled ✅",
        "Members will now keep their eligible roles when they leave and rejoin.",
    ), ctx.guild.id))


@sticky_group.command(name="disable", description="Turn OFF sticky roles for this server")
@commands.has_permissions(manage_roles=True)
async def sticky_disable(ctx: commands.Context) -> None:
    StickyRoleSystem.set_enabled(ctx.guild.id, False)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.warning(
        "Sticky Roles Disabled",
        "Roles will no longer be saved/restored on rejoin. Existing saved data is kept.",
    ), ctx.guild.id))


@sticky_group.command(name="add", description="Add a role to the sticky-eligible list")
@app_commands.describe(role="The role that should be saved/restored on rejoin")
@commands.has_permissions(manage_roles=True)
async def sticky_add(ctx: commands.Context, role: discord.Role) -> None:
    if role.managed:
        await ctx.send(embed=EmbedBuilder.error("Managed Role", "Integration/managed roles can't be sticky."), ephemeral=True)
        return
    eligible = StickyRoleSystem.get_eligible_roles(ctx.guild.id)
    if role.id not in eligible:
        eligible.append(role.id)
        StickyRoleSystem.set_eligible_roles(ctx.guild.id, eligible)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Sticky Role Added",
        f"{role.mention} is now sticky-eligible. {len(eligible)} role(s) on the list.",
    ), ctx.guild.id))


@sticky_group.command(name="remove", description="Remove a role from the sticky-eligible list")
@app_commands.describe(role="The role to remove from the sticky list")
@commands.has_permissions(manage_roles=True)
async def sticky_remove(ctx: commands.Context, role: discord.Role) -> None:
    eligible = StickyRoleSystem.get_eligible_roles(ctx.guild.id)
    if role.id in eligible:
        eligible.remove(role.id)
        StickyRoleSystem.set_eligible_roles(ctx.guild.id, eligible)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Sticky Role Removed",
        f"{role.mention} is no longer sticky-eligible. {len(eligible)} role(s) remain.",
    ), ctx.guild.id))


@sticky_group.command(name="list", description="Show all sticky-eligible roles")
@commands.has_permissions(manage_roles=True)
async def sticky_list(ctx: commands.Context) -> None:
    eligible = StickyRoleSystem.get_eligible_roles(ctx.guild.id)
    enabled = StickyRoleSystem.is_enabled(ctx.guild.id)
    embed = discord.Embed(
        title="📌 Sticky Roles",
        description=f"Status: **{'Enabled ✅' if enabled else 'Disabled ❌'}**\n"
                    f"Eligible roles: **{len(eligible)}**\n"
                    + ("_(No specific roles — all non-managed roles are saved.)_" if not eligible else ""),
        color=discord.Color.blurple(),
        timestamp=datetime.now(timezone.utc),
    )
    if eligible:
        lines = []
        for rid in eligible:
            r = ctx.guild.get_role(rid)
            lines.append(f"• {r.mention if r else f'~deleted:`{rid}`'}")
        embed.add_field(name="Eligible", value="\n".join(lines), inline=False)
    await ctx.send(embed=EmbedBuilder.branded(embed, ctx.guild.id))


@sticky_group.command(name="status", description="Show sticky-role status for the server or a member")
@app_commands.describe(member="Optional member to check their saved sticky roles")
@commands.has_permissions(manage_roles=True)
async def sticky_status(ctx: commands.Context, member: Optional[discord.Member] = None) -> None:
    enabled = StickyRoleSystem.is_enabled(ctx.guild.id)
    if member is None:
        await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.info(
            "Sticky Role Status",
            f"Enabled: **{'Yes' if enabled else 'No'}**\n"
            f"Eligible roles: **{len(StickyRoleSystem.get_eligible_roles(ctx.guild.id))}**",
        ), ctx.guild.id))
        return
    saved = data_manager.load_sticky_roles(ctx.guild.id, member.id)
    roles = [ctx.guild.get_role(r) for r in saved]
    lines = [r.mention if r else f"~deleted:`{r}`" for r in roles]
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.info(
        f"Sticky Roles for {member.display_name}",
        f"Saved roles: **{len(saved)}**\n" + ("\n".join(lines) if lines else "_(none saved)_"),
    ), ctx.guild.id))


# =========================================================================
# 3) FULL MESSAGE LOGGING — Dyno premium (edit/delete content)
# =========================================================================
class MessageLogSystem:
    @staticmethod
    def get_config(guild_id: int) -> Dict:
        return data_manager.get_message_log_config(guild_id)

    @staticmethod
    def save_config(cfg: Dict) -> None:
        data_manager.save_message_log_config(cfg)

    @staticmethod
    def is_channel_ignored(cfg: Dict, channel_id: int) -> bool:
        try:
            ignored = set(json.loads(cfg.get('ignore_channels', '[]') or '[]'))
        except Exception:
            ignored = set()
        return channel_id in ignored

    @staticmethod
    def should_log(cfg: Dict, message: discord.Message, kind: str) -> bool:
        if not cfg.get('enabled'):
            return False
        if cfg.get('ignore_bots', 1) and message.author.bot:
            return False
        if MessageLogSystem.is_channel_ignored(cfg, message.channel.id):
            return False
        if kind == 'edit' and not cfg.get('log_edits', 1):
            return False
        if kind == 'delete' and not cfg.get('log_deletes', 1):
            return False
        return True

    @staticmethod
    def cache(message: discord.Message) -> None:
        """Snapshot a message so we can recover its original content on edit/delete."""
        if message.guild is None:
            return
        cfg = MessageLogSystem.get_config(message.guild.id)
        if not cfg.get('enabled'):
            return
        # Don't cache messages in ignored channels.
        if MessageLogSystem.is_channel_ignored(cfg, message.channel.id):
            return
        try:
            attachments = []
            for a in message.attachments:
                attachments.append({
                    'filename': a.filename,
                    'url': a.url,
                    'proxy_url': getattr(a, 'proxy_url', None),
                    'size': getattr(a, 'size', None),
                })
            data_manager.cache_message({
                'message_id': message.id,
                'guild_id': message.guild.id,
                'channel_id': message.channel.id,
                'author_id': message.author.id,
                'author_name': str(message.author),
                'content': message.content or '',
                'attachments': json.dumps(attachments),
                'created_at': datetime.now(timezone.utc).isoformat(),
            })
        except Exception as exc:
            logging.debug(f"[MsgLog] cache failed: {exc}")

    @staticmethod
    async def log_delete(message: "discord.Message") -> None:
        if message.guild is None:
            return
        cfg = MessageLogSystem.get_config(message.guild.id)
        if not MessageLogSystem.should_log(cfg, message, 'delete'):
            return
        log_channel = message.guild.get_channel(cfg.get('log_channel_id') or 0)
        if log_channel is None:
            return

        # Prefer our cached original content (Discord's delete payload sometimes
        # still has it, but the cache is the reliable source of truth).
        cached = data_manager.load_cached_message(message.id)
        original_content = (cached['content'] if cached else None) or message.content or ''
        author_name = (cached['author_name'] if cached else None) or str(message.author)
        author_id = (cached['author_id'] if cached else None) or message.author.id

        snippet = original_content if len(original_content) <= 1024 else (original_content[:1021] + '...')

        embed = discord.Embed(
            title="🗑️ Message Deleted",
            description=(
                f"**Author:** <@{author_id}> (`{author_name}` / `{author_id}`)\n"
                f"**Channel:** {message.channel.mention}\n"
                f"**Message ID:** `{message.id}`"
            ),
            color=discord.Color.red(),
            timestamp=datetime.now(timezone.utc),
        )
        embed.add_field(
            name="Content",
            value=(snippet if snippet.strip() else "_(empty or embed-only message)_"),
            inline=False,
        )

        # Attachments.
        attachments = []
        if cached:
            try:
                attachments = json.loads(cached.get('attachments', '[]') or '[]')
            except Exception:
                attachments = []
        elif message.attachments:
            attachments = [{'filename': a.filename, 'url': a.url} for a in message.attachments]
        if attachments:
            att_lines = [f"• [{a.get('filename','file')}]({a.get('url')})" for a in attachments[:5]]
            embed.add_field(name="Attachments", value="\n".join(att_lines), inline=False)

        try:
            await log_channel.send(embed=EmbedBuilder.branded(embed, message.guild.id))
        except (discord.Forbidden, discord.HTTPException) as exc:
            logging.warning(f"[MsgLog] could not send delete log: {exc}")

        data_manager.delete_cached_message(message.id)

    @staticmethod
    async def log_bulk_delete(messages: List["discord.Message"], deleted_by: "discord.Member", channel: "discord.TextChannel", reason: str = "purge") -> None:
        """Log a batch of messages deleted via `!purge` / `!purgeall`.

        discord.py's `TextChannel.purge()` uses the bulk-delete endpoint,
        which does NOT fire `on_message_delete` for individual messages —
        so the normal MessageLogSystem.log_delete path never sees them.
        This helper is called from the purge command BEFORE the bulk delete
        so each purged message is recorded in the log channel.

        Sends a single compact summary embed (not one embed per message) so
        a `!purge 100` doesn't spam the log channel with 100 embeds.
        """
        if not messages:
            return
        if channel is None or channel.guild is None:
            return
        cfg = MessageLogSystem.get_config(channel.guild.id)
        if not MessageLogSystem.should_log(cfg, messages[0], 'delete'):
            return
        log_channel = channel.guild.get_channel(cfg.get('log_channel_id') or 0)
        if log_channel is None:
            return

        embed = discord.Embed(
            title=f"🧹 Bulk Purge — {len(messages)} message(s)",
            description=(
                f"**Moderator:** {deleted_by.mention} (`{deleted_by}` / `{deleted_by.id}`)\n"
                f"**Channel:** {channel.mention}\n"
                f"**Reason:** `{reason}`"
            ),
            color=discord.Color.orange(),
            timestamp=datetime.now(timezone.utc),
        )

        # Compact list of purged messages: one line each, truncated.
        # Discord embed field value limit is 1024 chars, so we cap the list
        # and note how many were truncated.
        MAX_FIELD = 1024
        lines: List[str] = []
        current_len = 0
        shown = 0
        truncated = 0
        for m in messages:
            author_disp = str(m.author)
            content = (m.content or '').replace('\n', ' ').strip()
            if not content and m.attachments:
                content = f"[attachment: {m.attachments[0].filename}]"
            if not content:
                content = "(empty)"
            if len(content) > 80:
                content = content[:77] + '...'
            line = f"`{m.id}` **{author_disp}**: {content}"
            line_len = len(line) + 1
            if current_len + line_len > MAX_FIELD - 20 and shown >= 1:
                # Stop adding lines to keep under the field limit.
                truncated = len(messages) - shown
                break
            lines.append(line)
            current_len += line_len
            shown += 1

        list_text = "\n".join(lines)
        if truncated > 0:
            list_text += f"\n..._and {truncated} more_"

        embed.add_field(
            name=f"Deleted messages ({len(messages)} total)",
            value=list_text or "_(no content)_",
            inline=False,
        )

        # Mention the bulk-delete behavior so log readers know individual
        # on_message_delete events did NOT fire for these.
        embed.set_footer(text="Bulk-purged via command • individual delete events were not fired by Discord")

        try:
            await log_channel.send(embed=EmbedBuilder.branded(embed, channel.guild.id))
        except (discord.Forbidden, discord.HTTPException) as exc:
            logging.warning(f"[MsgLog] could not send bulk-purge log: {exc}")

        # Also delete the cached copies of the purged messages so the cache
        # doesn't grow stale with rows for messages that no longer exist.
        for m in messages:
            try:
                data_manager.delete_cached_message(m.id)
            except Exception:
                pass

    @staticmethod
    async def log_edit(before: "discord.Message", after: "discord.Message") -> None:
        if before.guild is None:
            return
        cfg = MessageLogSystem.get_config(before.guild.id)
        if not MessageLogSystem.should_log(cfg, before, 'edit'):
            return
        # No-op if content didn't change (pin edits etc. fire on_message_edit).
        if (before.content or '') == (after.content or ''):
            return
        log_channel = before.guild.get_channel(cfg.get('log_channel_id') or 0)
        if log_channel is None:
            return

        old_content = before.content or ''
        new_content = after.content or ''
        old_snip = old_content if len(old_content) <= 1024 else (old_content[:1021] + '...')
        new_snip = new_content if len(new_content) <= 1024 else (new_content[:1021] + '...')

        embed = discord.Embed(
            title="✏️ Message Edited",
            description=(
                f"**Author:** {before.author.mention} (`{before.author.id}`)\n"
                f"**Channel:** {before.channel.mention}\n"
                f"**Message ID:** `{before.id}` — [jump]({after.jump_url})"
            ),
            color=discord.Color.orange(),
            timestamp=datetime.now(timezone.utc),
        )
        embed.add_field(name="Before", value=(old_snip if old_snip.strip() else "_(empty)_"), inline=False)
        embed.add_field(name="After", value=(new_snip if new_snip.strip() else "_(empty)_"), inline=False)
        try:
            await log_channel.send(embed=EmbedBuilder.branded(embed, before.guild.id))
        except (discord.Forbidden, discord.HTTPException) as exc:
            logging.warning(f"[MsgLog] could not send edit log: {exc}")

        # Refresh the cache with the new content.
        MessageLogSystem.cache(after)


@bot.hybrid_group(name="msglog", description="Full Message Logging (Dyno premium — edits + deletes with content)")
@app_commands.default_permissions(manage_guild=True)
async def msglog_group(ctx: commands.Context) -> None:
    if ctx.invoked_subcommand is None:
        await ctx.send(embed=EmbedBuilder.info(
            "Message Logging",
            "Subcommands: `enable`, `disable`, `channel`, `edits`, `deletes`, `ignore`, `status`.",
        ), ephemeral=True)


@msglog_group.command(name="enable", description="Turn ON full message logging")
@app_commands.describe(channel="The channel to send logs to")
@commands.has_permissions(manage_guild=True)
async def msglog_enable(ctx: commands.Context, channel: discord.TextChannel) -> None:
    cfg = MessageLogSystem.get_config(ctx.guild.id)
    cfg['enabled'] = 1
    cfg['log_channel_id'] = channel.id
    cfg['updated_at'] = datetime.now(timezone.utc).isoformat()
    MessageLogSystem.save_config(cfg)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Message Logging Enabled ✅",
        f"Edits + deletes will be logged to {channel.mention}.\n_(Bot messages are ignored by default.)_",
    ), ctx.guild.id))


@msglog_group.command(name="disable", description="Turn OFF full message logging")
@commands.has_permissions(manage_guild=True)
async def msglog_disable(ctx: commands.Context) -> None:
    cfg = MessageLogSystem.get_config(ctx.guild.id)
    cfg['enabled'] = 0
    cfg['updated_at'] = datetime.now(timezone.utc).isoformat()
    MessageLogSystem.save_config(cfg)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.warning(
        "Message Logging Disabled", "Edits and deletes will no longer be logged."
    ), ctx.guild.id))


@msglog_group.command(name="channel", description="Change the log channel")
@app_commands.describe(channel="The channel to send logs to")
@commands.has_permissions(manage_guild=True)
async def msglog_channel(ctx: commands.Context, channel: discord.TextChannel) -> None:
    cfg = MessageLogSystem.get_config(ctx.guild.id)
    cfg['log_channel_id'] = channel.id
    cfg['updated_at'] = datetime.now(timezone.utc).isoformat()
    MessageLogSystem.save_config(cfg)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Log Channel Updated", f"Logs will go to {channel.mention}."
    ), ctx.guild.id))


@msglog_group.command(name="edits", description="Toggle edit logging on/off")
@app_commands.describe(enabled="True to log edits, False to stop")
@commands.has_permissions(manage_guild=True)
async def msglog_edits(ctx: commands.Context, enabled: bool) -> None:
    cfg = MessageLogSystem.get_config(ctx.guild.id)
    cfg['log_edits'] = 1 if enabled else 0
    cfg['updated_at'] = datetime.now(timezone.utc).isoformat()
    MessageLogSystem.save_config(cfg)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Edit Logging Updated", f"Edit logging is now **{'ON' if enabled else 'OFF'}**."
    ), ctx.guild.id))


@msglog_group.command(name="deletes", description="Toggle delete logging on/off")
@app_commands.describe(enabled="True to log deletes, False to stop")
@commands.has_permissions(manage_guild=True)
async def msglog_deletes(ctx: commands.Context, enabled: bool) -> None:
    cfg = MessageLogSystem.get_config(ctx.guild.id)
    cfg['log_deletes'] = 1 if enabled else 0
    cfg['updated_at'] = datetime.now(timezone.utc).isoformat()
    MessageLogSystem.save_config(cfg)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Delete Logging Updated", f"Delete logging is now **{'ON' if enabled else 'OFF'}**."
    ), ctx.guild.id))


@msglog_group.command(name="ignore", description="Add/remove a channel from the ignore list")
@app_commands.describe(channel="The channel to toggle ignore status for")
@commands.has_permissions(manage_guild=True)
async def msglog_ignore(ctx: commands.Context, channel: discord.TextChannel) -> None:
    cfg = MessageLogSystem.get_config(ctx.guild.id)
    try:
        ignored = json.loads(cfg.get('ignore_channels', '[]') or '[]')
    except Exception:
        ignored = []
    if channel.id in ignored:
        ignored.remove(channel.id)
        state = "no longer ignored"
    else:
        ignored.append(channel.id)
        state = "now ignored"
    cfg['ignore_channels'] = json.dumps(ignored)
    cfg['updated_at'] = datetime.now(timezone.utc).isoformat()
    MessageLogSystem.save_config(cfg)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Ignore List Updated", f"{channel.mention} is **{state}**."
    ), ctx.guild.id))


@msglog_group.command(name="status", description="Show the current message-logging configuration")
@commands.has_permissions(manage_guild=True)
async def msglog_status(ctx: commands.Context) -> None:
    cfg = MessageLogSystem.get_config(ctx.guild.id)
    chan = ctx.guild.get_channel(cfg.get('log_channel_id') or 0)
    try:
        ignored = json.loads(cfg.get('ignore_channels', '[]') or '[]')
    except Exception:
        ignored = []
    embed = discord.Embed(
        title="📝 Message Logging Status",
        description=(
            f"Enabled: **{'Yes ✅' if cfg.get('enabled') else 'No ❌'}**\n"
            f"Log channel: {chan.mention if chan else '_(not set)_'}\n"
            f"Log edits: **{'Yes' if cfg.get('log_edits', 1) else 'No'}**\n"
            f"Log deletes: **{'Yes' if cfg.get('log_deletes', 1) else 'No'}**\n"
            f"Ignore bots: **{'Yes' if cfg.get('ignore_bots', 1) else 'No'}**\n"
            f"Ignored channels: **{len(ignored)}**"
        ),
        color=discord.Color.blurple(),
        timestamp=datetime.now(timezone.utc),
    )
    if ignored:
        lines = [ctx.guild.get_channel(c).mention if ctx.guild.get_channel(c) else f"`{c}`" for c in ignored]
        embed.add_field(name="Ignored", value="\n".join(lines), inline=False)
    await ctx.send(embed=EmbedBuilder.branded(embed, ctx.guild.id))


# =========================================================================
# 4) CUSTOM BOT BRANDING — avatar / banner / footer (premium feel)
# =========================================================================
@bot.hybrid_group(name="botbranding", aliases=["branding"], description="Custom Bot Branding (avatar / banner / footer)")
@app_commands.default_permissions(manage_guild=True)
async def branding_group(ctx: commands.Context) -> None:
    if ctx.invoked_subcommand is None:
        await ctx.send(embed=EmbedBuilder.info(
            "Custom Bot Branding",
            "Subcommands: `footer`, `color`, `thumbnail`, `image`, `banner`, `avatar`, `view`, `clear`.",
        ), ephemeral=True)


@branding_group.command(name="footer", description="Set a custom embed footer for this server")
@app_commands.describe(text="Footer text (supports [GANG NAME] / [GANG ABBR] placeholders)")
@commands.has_permissions(manage_guild=True)
async def branding_footer(ctx: commands.Context, *, text: str) -> None:
    b = data_manager.get_branding(ctx.guild.id)
    b['embed_footer'] = text[:300]
    b['updated_at'] = datetime.now(timezone.utc).isoformat()
    data_manager.save_branding(b)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Footer Updated",
        f"Embeds will now show:\n`{brand_text(text)}`\n_(applies to new reaction-role / sticky / msglog embeds)_",
    ), ctx.guild.id))


@branding_group.command(name="color", description="Set a custom embed color (hex)")
@app_commands.describe(color="Hex color, e.g. #FF6B35 or 0xFF6B35")
@commands.has_permissions(manage_guild=True)
async def branding_color(ctx: commands.Context, color: str) -> None:
    color_int = _hex_to_int(color)
    if color_int is None:
        await ctx.send(embed=EmbedBuilder.error("Bad Color", "Use a 6-digit hex like `#FF6B35`."), ephemeral=True)
        return
    b = data_manager.get_branding(ctx.guild.id)
    b['embed_color'] = color_int
    b['updated_at'] = datetime.now(timezone.utc).isoformat()
    data_manager.save_branding(b)
    preview = discord.Embed(title="Color Preview", description=f"`{color}` → `#{color_int:06X}`", color=discord.Color(color_int))
    await ctx.send(embed=EmbedBuilder.branded(preview, ctx.guild.id))


@branding_group.command(name="thumbnail", description="Set a custom embed thumbnail URL")
@app_commands.describe(url="Direct image URL")
@commands.has_permissions(manage_guild=True)
async def branding_thumbnail(ctx: commands.Context, url: str) -> None:
    if not url.startswith(("http://", "https://")):
        await ctx.send(embed=EmbedBuilder.error("Bad URL", "Thumbnail must be an http(s) URL."), ephemeral=True)
        return
    b = data_manager.get_branding(ctx.guild.id)
    b['embed_thumbnail'] = url
    b['updated_at'] = datetime.now(timezone.utc).isoformat()
    data_manager.save_branding(b)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Thumbnail Updated", "New embeds will use this thumbnail."
    ), ctx.guild.id))


@branding_group.command(name="image", description="Set a custom embed image URL")
@app_commands.describe(url="Direct image URL")
@commands.has_permissions(manage_guild=True)
async def branding_image(ctx: commands.Context, url: str) -> None:
    if not url.startswith(("http://", "https://")):
        await ctx.send(embed=EmbedBuilder.error("Bad URL", "Image must be an http(s) URL."), ephemeral=True)
        return
    b = data_manager.get_branding(ctx.guild.id)
    b['embed_image'] = url
    b['updated_at'] = datetime.now(timezone.utc).isoformat()
    data_manager.save_branding(b)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Image Updated", "New embeds will use this image."
    ), ctx.guild.id))


@branding_group.command(name="banner", description="Set a banner image used in branding embeds")
@app_commands.describe(url="Direct image URL")
@commands.has_permissions(manage_guild=True)
async def branding_banner(ctx: commands.Context, url: str) -> None:
    if not url.startswith(("http://", "https://")):
        await ctx.send(embed=EmbedBuilder.error("Bad URL", "Banner must be an http(s) URL."), ephemeral=True)
        return
    b = data_manager.get_branding(ctx.guild.id)
    b['banner_url'] = url
    b['updated_at'] = datetime.now(timezone.utc).isoformat()
    data_manager.save_branding(b)
    await ctx.send(embed=EmbedBuilder.branded(EmbedBuilder.success(
        "Banner Updated", "Banner is shown on `/botbranding view`."
    ), ctx.guild.id))


@branding_group.command(name="avatar", description="Change the bot's actual avatar (owner only)")
@app_commands.describe(url="Direct image URL (png/jpg/gif)")
@commands.is_owner()
async def branding_avatar(ctx: commands.Context, url: str) -> None:
    if not url.startswith(("http://", "https://")):
        await ctx.send(embed=EmbedBuilder.error("Bad URL", "Avatar must be an http(s) URL."), ephemeral=True)
        return
    await ctx.defer()
    image_bytes = await _fetch_image_bytes(url)
    if not image_bytes:
        await ctx.send(embed=EmbedBuilder.error("Download Failed", "Could not fetch the image."), ephemeral=True)
        return
    try:
        await bot.user.edit(avatar=image_bytes)
        await ctx.send(embed=EmbedBuilder.success(
            "Bot Avatar Updated ✅",
            "The bot's avatar has been changed. It may take a moment to propagate in Discord.",
        ))
    except discord.HTTPException as exc:
        if 'You are being rate limited' in str(exc) or 'rate limit' in str(exc).lower():
            await ctx.send(embed=EmbedBuilder.error("Rate Limited", "Discord limits avatar changes to ~2/hour. Try again later."), ephemeral=True)
        else:
            await ctx.send(embed=EmbedBuilder.error("Avatar Update Failed", f"Discord rejected the image: `{exc}`"), ephemeral=True)
    except Exception as exc:
        await ctx.send(embed=EmbedBuilder.error("Avatar Update Failed", f"`{exc}`"), ephemeral=True)


@branding_group.command(name="view", description="View the current branding for this server")
@commands.has_permissions(manage_guild=True)
async def branding_view(ctx: commands.Context) -> None:
    b = data_manager.get_branding(ctx.guild.id)
    embed = discord.Embed(
        title=f"🎨 Branding — {ctx.guild.name}",
        description=(
            f"**Footer:** {b.get('embed_footer') or '_(not set)_'}\n"
            f"**Color:** {('#%06X' % b['embed_color']) if isinstance(b.get('embed_color'), int) else '_(not set)_'}\n"
            f"**Thumbnail:** [link]({b['embed_thumbnail']})" if b.get('embed_thumbnail') else "**Thumbnail:** _(not set)_"
        ),
        color=(discord.Color(b['embed_color']) if isinstance(b.get('embed_color'), int) else discord.Color.blurple()),
        timestamp=datetime.now(timezone.utc),
    )
    if b.get('embed_thumbnail'):
        embed.set_thumbnail(url=b['embed_thumbnail'])
    if b.get('embed_image'):
        embed.set_image(url=b['embed_image'])
    elif b.get('banner_url'):
        embed.set_image(url=b['banner_url'])
    await ctx.send(embed=embed)


@branding_group.command(name="clear", description="Reset all branding for this server")
@commands.has_permissions(manage_guild=True)
async def branding_clear(ctx: commands.Context) -> None:
    data_manager.save_branding({
        'guild_id': ctx.guild.id,
        'embed_footer': None, 'embed_color': None,
        'embed_thumbnail': None, 'embed_image': None,
        'avatar_url': None, 'banner_url': None,
        'updated_at': datetime.now(timezone.utc).isoformat(),
    })
    await ctx.send(embed=EmbedBuilder.success("Branding Cleared", "This server will use the bot's default styling."))


# =========================================================================
# EVENT HANDLERS for the four premium features
# =========================================================================

@bot.event
async def on_raw_reaction_add(payload: "discord.RawReactionActionEvent") -> None:
    # Ignore reactions on the bot's own messages handled elsewhere (welcome etc.)
    try:
        await ReactionRoleSystem.handle_reaction(payload, added=True)
    except Exception as exc:
        logging.exception(f"[RR] on_raw_reaction_add error: {exc}")


@bot.event
async def on_raw_reaction_remove(payload: "discord.RawReactionActionEvent") -> None:
    try:
        # payload.member is None on reaction_remove — fetch the member.
        if payload.member is None and payload.guild_id is not None:
            guild = bot.get_guild(payload.guild_id)
            if guild is not None:
                payload.member = guild.get_member(payload.user_id)
        await ReactionRoleSystem.handle_reaction(payload, added=False)
    except Exception as exc:
        logging.exception(f"[RR] on_raw_reaction_remove error: {exc}")


@bot.event
async def on_raw_reaction_clear(payload: "discord.RawReactionClearEvent") -> None:
    """When all reactions are cleared from a message, drop its RR mappings."""
    try:
        removed = data_manager.delete_reaction_roles_for_message(payload.message_id)
        if removed:
            logging.info(f"[RR] Cleared {removed} mappings (reactions cleared on msg {payload.message_id})")
    except Exception as exc:
        logging.exception(f"[RR] on_raw_reaction_clear error: {exc}")


@bot.event
async def on_message_delete(message: discord.Message) -> None:
    try:
        # Drop any reaction-role mappings tied to the deleted message.
        data_manager.delete_reaction_roles_for_message(message.id)
        # Full message logging.
        await MessageLogSystem.log_delete(message)
    except Exception as exc:
        logging.exception(f"[MsgLog] on_message_delete error: {exc}")


@bot.event
async def on_message_edit(before: discord.Message, after: discord.Message) -> None:
    try:
        await MessageLogSystem.log_edit(before, after)
    except Exception as exc:
        logging.exception(f"[MsgLog] on_message_edit error: {exc}")


@bot.event
async def on_member_remove(member: discord.Member) -> None:
    try:
        StickyRoleSystem.capture_member_roles(member)
    except Exception as exc:
        logging.exception(f"[Sticky] on_member_remove error: {exc}")

@bot.hybrid_command(name="cmds", description="Show all available commands (paginated)")
async def cmds(ctx: commands.Context) -> None:
    """Shows the paginated help menu."""
    embeds = build_help_pages()
    
    # Check if this command was part of a multi-command chain
    # Tuple format: (page_offset, is_chained)
    initial_page, is_chained = _chain_offsets.pop(ctx.message.id, (0, False))
    
    # If it's chained, send JUST the embed with no buttons
    if is_chained:
        await ctx.send(embed=embeds[initial_page])
        return
        
    # Otherwise, send the normal interactive pagination
    view = PaginatedHelpView(ctx.author.id, embeds, initial_page=initial_page)
    view.message = await ctx.send(embed=view.current_embed(), view=view)


# --- ERROR HANDLING ---
@bot.event
async def on_command_error(ctx: commands.Context, error: Exception) -> None:
    if isinstance(error, commands.MissingPermissions):
        await ctx.send("You don't have permission to use this command.")
    elif isinstance(error, commands.MissingRequiredArgument):
        await ctx.send("Missing required argument.")
    elif isinstance(error, commands.BadArgument):
        await ctx.send("Invalid argument provided.")
    elif isinstance(error, commands.CommandOnCooldown):
        await ctx.send(f"Cooldown. Try again in {error.retry_after:.0f} seconds.")
    else:
        logging.error(f'Error: {str(error)}')
        await ctx.send(f"An error occurred: {str(error)}")

# ------------------------------------------------------------------
# Slash command global error handler -- surfaces failures in logs.
# ------------------------------------------------------------------
@bot.tree.error
async def on_app_command_error(
    interaction: discord.Interaction,
    error: app_commands.AppCommandError,
) -> None:
    logging.exception("[Slash Command] error: %s", error)
    try:
        if interaction.response.is_done():
            await interaction.followup.send(
                f"Command failed: `{error}`", ephemeral=True
            )
        else:
            await interaction.response.send_message(
                f"Command failed: `{error}`", ephemeral=True
            )
    except discord.HTTPException:
        pass


# ------------------------------------------------------------------
# Owner-only manual sync command.
# Usage:
#   !sync          -> global sync (may take up to 1 hour to appear)
#   !sync <guild>  -> per-guild sync (instant, great for testing)
# ------------------------------------------------------------------
@bot.hybrid_command(name="sync", description="Re-sync slash commands (owner only)")
@commands.is_owner()
@app_commands.describe(
    guild_id="Optional guild id for instant per-guild sync. Omit for global sync."
)
async def sync_cmd(ctx: commands.Context, guild_id: Optional[str] = None) -> None:
    """Re-sync slash commands to Discord."""
    try:
        if guild_id:
            try:
                gid = int(guild_id)
            except ValueError:
                await ctx.send("guild_id must be a numeric Discord guild id.")
                return
            target = discord.Object(id=gid)
            # For per-guild sync we must CLEAR then SYNC to remove deleted cmds.
            bot.tree.clear_commands(guild=target)
            synced = await bot.tree.sync(guild=target)
            await ctx.send(embed=EmbedBuilder.success(
                "Slash Commands Synced (Guild)",
                f"Synced {len(synced)} commands to guild {gid} (instant)."
            ))
        else:
            synced = await bot.tree.sync()
            await ctx.send(embed=EmbedBuilder.success(
                "Slash Commands Synced (Global)",
                f"Synced {len(synced)} commands globally.\n"
                "Note: global sync can take up to 1 hour to appear in all clients."
            ))
        logging.info(f"[Slash Commands] Manual sync by {ctx.author}: "
                     f"{len(synced)} cmds {'guild='+guild_id if guild_id else 'global'}")
    except Exception as e:
        logging.exception("[Slash Commands] Manual sync failed: %s", e)
        await ctx.send(f"Failed to sync: `{e}`")


# --- GETALLROLES AUTO-UPDATE EVENTS ---
# Whenever a role is created, deleted, or its name changes, refresh every
# active getallroles embed in that guild so the lists stay live without the
# owner having to re-run the command.
@bot.event
async def on_guild_role_create(role: discord.Role) -> None:
    # Avoid running during startup before data_manager is ready.
    if 'data_manager' not in globals() or data_manager is None:
        return
    try:
        await refresh_getallroles_messages(role.guild)
    except Exception as e:
        logging.warning(f"[GetAllRoles] on_guild_role_create refresh failed: {e}")


@bot.event
async def on_guild_role_delete(role: discord.Role) -> None:
    if 'data_manager' not in globals() or data_manager is None:
        return
    try:
        await refresh_getallroles_messages(role.guild)
    except Exception as e:
        logging.warning(f"[GetAllRoles] on_guild_role_delete refresh failed: {e}")


@bot.event
async def on_guild_role_update(before: discord.Role, after: discord.Role) -> None:
    # Only refresh if something visible to the embed changed (name or
    # position). Other updates (permissions, color) don't affect the list.
    if before.name != after.name or before.position != after.position:
        if 'data_manager' not in globals() or data_manager is None:
            return
        try:
            await refresh_getallroles_messages(after.guild)
        except Exception as e:
            logging.warning(f"[GetAllRoles] on_guild_role_update refresh failed: {e}")


def build_getallroles_embed(guild: discord.Guild) -> discord.Embed:
    """Build the All-Roles embed for a guild.

    Shared between the command, the auto-update handlers, and the startup
    refresh so every code path renders the embed identically. All roles are
    packed into a single embed (auto-split into multiple code blocks if the
    list would exceed Discord's 4096-char description limit). No pagination.
    """
    roles = sorted(guild.roles, key=lambda r: r.position, reverse=True)

    lines: List[str] = []
    for role in roles:
        if role.is_default():
            lines.append(f"@everyone - {role.id}")
        else:
            lines.append(f"{role.name} - {role.id}")

    embed = discord.Embed(
        title=f"📋 All Roles in {guild.name}",
        description=f"**{len(roles)} role(s) total**",
        color=discord.Color.blurple(),
        timestamp=datetime.now(timezone.utc),
    )

    if not roles:
        embed.description = "No roles found in this server."
        return embed

    # Discord embed description max = 4096 chars. Each code block needs
    # ```\n ... \n``` wrappers (8 chars overhead). Pack as many roles as
    # possible into each block, splitting into multiple blocks if needed —
    # but always within a SINGLE embed (no pagination buttons).
    MAX_DESC = 4096
    CODE_FENCE_OVERHEAD = 8

    blocks: List[str] = []
    current_block: List[str] = []
    current_len = 0

    for line in lines:
        line_len = len(line) + 1
        if current_len + line_len + CODE_FENCE_OVERHEAD > MAX_DESC - len(blocks) * 2 and current_block:
            blocks.append("```\n" + "\n".join(current_block) + "\n```")
            current_block = []
            current_len = 0
        current_block.append(line)
        current_len += line_len

    if current_block:
        blocks.append("```\n" + "\n".join(current_block) + "\n```")

    if len(blocks) == 1:
        embed.description = f"**{len(roles)} role(s) total**\n{blocks[0]}"
    else:
        embed.description = f"**{len(roles)} role(s) total** — split into {len(blocks)} blocks"
        for i, block in enumerate(blocks[:25]):  # Discord max 25 fields
            embed.add_field(
                name=f"Roles (part {i + 1}/{len(blocks)})",
                value=block,
                inline=False,
            )

    embed.set_footer(text="📋 Click Copy List to copy all roles • Auto-updates on role changes")
    return embed


def build_getallroles_plain_text(guild: discord.Guild) -> str:
    """Build a plain-text (no markdown) list of all roles + IDs.

    Used by the Copy List button so the ephemeral response is easy to
    select-all-and-copy. One role per line: `RoleName - ID`.
    """
    roles = sorted(guild.roles, key=lambda r: r.position, reverse=True)
    parts: List[str] = []
    for role in roles:
        if role.is_default():
            parts.append(f"@everyone - {role.id}")
        else:
            parts.append(f"{role.name} - {role.id}")
    return "\n".join(parts) if parts else "No roles found."


async def refresh_getallroles_messages(guild: discord.Guild) -> None:
    """Rebuild and edit every active getallroles embed for `guild`.

    Called from on_guild_role_create / on_guild_role_delete /
    on_guild_role_update so the lists stay live. Messages that no longer
    exist are pruned from the tracking table.
    """
    tracked = data_manager.load_getallroles_messages(guild_id=guild.id)
    if not tracked:
        return

    embed = build_getallroles_embed(guild)
    view = GetAllRolesView()

    for row in tracked:
        channel = guild.get_channel(row['channel_id'])
        if channel is None:
            # Channel was deleted — stop tracking this embed.
            data_manager.delete_getallroles_message(row['message_id'])
            continue
        try:
            message = await channel.fetch_message(row['message_id'])
        except (discord.NotFound, discord.Forbidden, discord.HTTPException):
            # Message was deleted or we lost access — stop tracking it.
            data_manager.delete_getallroles_message(row['message_id'])
            continue
        try:
            await message.edit(embed=embed, view=view)
        except (discord.HTTPException, discord.Forbidden):
            # Edit failed (e.g. channel now read-only). Leave it tracked;
            # a future role change will retry, or it'll be pruned if the
            # message is gone.
            pass


class GetAllRolesView(View):
    """Persistent view attached to every getallroles embed.

    Carries a single 'Copy List' button that DMs the clicker the full role
    list as plain text in a code block so they can select-all-and-copy.
    Persisted via bot.add_view() on startup so the button keeps working
    after a bot restart.
    """

    def __init__(self):
        super().__init__(timeout=None)

    @discord.ui.button(label="Copy List", style=discord.ButtonStyle.secondary, emoji="📋", custom_id="getallroles_copy")
    async def copy_button(self, interaction: discord.Interaction, button: Button) -> None:
        if not interaction.guild:
            await interaction.response.send_message("This can only be used in a server.", ephemeral=True)
            return
        text = build_getallroles_plain_text(interaction.guild)

        # Discord message content limit is 2000 chars. Code fence adds 8.
        # If the list fits in one message, send it as a single code block.
        # Otherwise split across multiple ephemeral followups.
        MAX_MSG = 2000
        CODE_OVERHEAD = 8  # "```\n" + "\n```"
        CHUNK = MAX_MSG - CODE_OVERHEAD  # ~1992 chars per code block

        await interaction.response.defer(ephemeral=True)

        if len(text) <= CHUNK:
            await interaction.followup.send(f"```\n{text}\n```", ephemeral=True)
            return

        # Split on newline boundaries so we never cut a role line in half.
        lines = text.split("\n")
        chunks: List[str] = []
        current: List[str] = []
        current_len = 0
        for line in lines:
            line_len = len(line) + 1
            if current_len + line_len > CHUNK and current:
                chunks.append("\n".join(current))
                current = []
                current_len = 0
            current.append(line)
            current_len += line_len
        if current:
            chunks.append("\n".join(current))

        total = len(chunks)
        for i, chunk in enumerate(chunks, 1):
            header = f"**All roles ({i}/{total})**\n" if total > 1 else ""
            await interaction.followup.send(f"{header}```\n{chunk}\n```", ephemeral=True)


@bot.hybrid_command(name="getallroles", aliases=["roles", "listroles"], description="Get a list of all roles and their IDs (owner only)")
@commands.is_owner()
@commands.guild_only()
async def getallroles_cmd(ctx: commands.Context) -> None:
    """Display all roles in the server with their IDs on a single list.

    The embed is auto-updated whenever a role is created, deleted, or renamed
    in this server — no need to re-run the command. A persistent Copy List
    button sends the full list to your DMs (ephemeral) for easy copy-paste.
    The embed and button both survive a bot restart.
    """
    embed = build_getallroles_embed(ctx.guild)
    view = GetAllRolesView()
    message = await ctx.send(embed=embed, view=view)

    # Track the message so role-create/delete/update events can refresh it
    # and on_ready can re-attach the persistent view after a restart.
    try:
        data_manager.save_getallroles_message(message.id, ctx.channel.id, ctx.guild.id, ctx.author.id)
    except Exception as e:
        logging.warning(f"[GetAllRoles] Could not track message {message.id}: {e}")


# ------------------------------------------------------------------
# Owner-only command to re-send the setup tutorial on demand.
# ------------------------------------------------------------------
@bot.hybrid_command(name="tutorial", description="Re-send the bot setup tutorial to your DMs (owner only)")
@commands.is_owner()
async def tutorial_cmd(ctx: commands.Context) -> None:
    """Re-send the full setup tutorial to the owner's DMs."""
    await ctx.send(embed=EmbedBuilder.success(
        "Tutorial Sent",
        "Check your DMs — the full setup tutorial is on its way.\n"
        "_(If you didn't receive it, your DMs may be closed. Enable DMs from server members and try again.)_"
    ), ephemeral=True)
    await send_owner_tutorial(force=True)
    logging.info(f"[Tutorial] Manually re-sent by owner {ctx.author}")



    # --- TOKEN & STARTUP ---
def load_local_env_file(env_path: Optional[str] = None) -> None:
    if env_path is None:
        env_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), '..', '.env')

    try:
        if not os.path.exists(env_path):
            return

        with open(env_path, 'r', encoding='utf-8') as f:
            for raw_line in f:
                line = raw_line.strip()
                if not line or line.startswith('#') or '=' not in line:
                    continue

                key, value = line.split('=', 1)
                key = key.strip()
                value = value.strip().strip('"').strip("'")

                if key and key not in os.environ:
                    os.environ[key] = value
    except Exception as exc:
        logging.warning(f"[Startup] Could not load env file {env_path}: {exc}")


def is_placeholder_token(token: Optional[str]) -> bool:
    if not token:
        return True

    normalized = token.strip().lower()
    return normalized in {"", "your_bot_token_here", "your_discord_bot_token_here", "changeme", "replace_me"}


def read_token_from_file(token_name: str = 'BOT_Token') -> Optional[str]:
    script_dir = os.path.dirname(os.path.abspath(__file__))
    search_paths = [
        os.path.join(script_dir, 'tokens.txt'),
        os.path.join(script_dir, '..', 'tokens.txt'),
        os.path.join(os.getcwd(), 'tokens.txt'),
        'tokens.txt',
    ]

    for filepath in search_paths:
        try:
            if os.path.exists(filepath):
                with open(filepath, 'r', encoding='utf-8') as f:
                    for line in f:
                        line = line.strip()
                        if line.startswith(f"{token_name}="):
                            logging.info(f"Token found in: {os.path.abspath(filepath)}")
                            return line.split('=', 1)[1].strip()
        except Exception:
            continue

    return None


def get_bot_token() -> Optional[str]:
    load_local_env_file()

    for env_name in ('BOT_TOKEN', 'DISCORD_TOKEN', 'BOT_Token', 'TOKEN'):
        token_value = os.environ.get(env_name)
        if token_value and not is_placeholder_token(token_value):
            return token_value

    return read_token_from_file('BOT_Token')


# --- MAIN ENTRY POINT ---
TOKEN = get_bot_token()

if TOKEN is None or is_placeholder_token(TOKEN):
    print("=" * 50)
    print("ERROR: Bot token not found!")
    print("Create a .env file in the project root with:")
    print("BOT_TOKEN=YOUR_BOT_TOKEN_HERE")
    print("or create 'tokens.txt' in the project root with:")
    print("BOT_Token=YOUR_BOT_TOKEN_HERE")
    print("=" * 50)
    logging.error("Bot token not found. Please create a .env file or tokens.txt")
    exit(1)


if __name__ == "__main__":
    print("=" * 50)
    print(f"{config.gang_name} BOT - Starting (By IdkAnymore_039)")
    print("=" * 50)
    try:
        bot.run(TOKEN)
    finally:
        save_all_data()
        process_manager.clear_lock_file()