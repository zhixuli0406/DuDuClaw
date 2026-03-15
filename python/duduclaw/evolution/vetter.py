"""Skill Vetter - Security scanning for agent-generated skills

Note: skill_security_scan should be enabled by default in the Rust onboard
configuration to ensure all agent-generated skills are vetted before activation.
"""
import logging
import re
from dataclasses import dataclass
from enum import Enum
from pathlib import Path
from typing import List, Optional, Tuple

logger = logging.getLogger(__name__)


class VetterResult(Enum):
    PASS = "pass"
    WARN = "warn"
    FAIL = "fail"


@dataclass
class SecurityFinding:
    category: str
    severity: VetterResult
    description: str
    line_number: Optional[int] = None
    pattern_matched: Optional[str] = None


# Pre-compiled pattern definitions: (category, severity, compiled_patterns)
_PATTERN_DEFS: List[Tuple[str, VetterResult, List[str]]] = [
    ("command_injection", VetterResult.FAIL, [
        r'rm\s+-rf\s+/',
        r'curl\s+.*\|\s*sh',
        r'curl\s+.*\|\s*bash',
        r'wget\s+.*\|\s*sh',
        r'\beval\b\s*\(',
        r'exec\s*\(',
        r'os\.system\s*\(',
        r'subprocess\.call\s*\(',
        r'`[^`]+`',  # backtick execution
    ]),
    ("path_traversal", VetterResult.FAIL, [
        r'\.\./\.\.',
        r'/etc/(passwd|shadow|hosts)',
        r'~/(\.ssh|\.gnupg|\.env)',
        r'/root/',
        r'C:\\Windows\\System32',
    ]),
    ("prompt_injection", VetterResult.WARN, [
        r'ignore\s+(previous|above|all)\s+instructions',
        r'disregard\s+(previous|above|all)',
        r'system\s*:\s*you\s+are',
        r'act\s+as\s+if\s+you',
        r'pretend\s+you\s+are',
    ]),
    ("resource_abuse", VetterResult.WARN, [
        r'while\s+true',
        r'for\s*\(\s*;\s*;\s*\)',
        r'loop\s*\{',
        r'sleep\s*\(\s*0\s*\)',
    ]),
    ("sensitive_data", VetterResult.FAIL, [
        r'(api[_-]?key|apikey)\s*[=:]\s*["\'][^"\']+["\']',
        r'(password|passwd|pwd)\s*[=:]\s*["\'][^"\']+["\']',
        r'(secret|token)\s*[=:]\s*["\'][^"\']+["\']',
        r'sk-[a-zA-Z0-9]{20,}',  # API key pattern
        r'ghp_[a-zA-Z0-9]{36}',  # GitHub token
    ]),
]

# Pre-compile all patterns at module load time
_COMPILED_PATTERNS: List[Tuple[str, VetterResult, List[re.Pattern]]] = [
    (category, severity, [re.compile(p, re.IGNORECASE) for p in patterns])
    for category, severity, patterns in _PATTERN_DEFS
]


class SkillVetter:
    """Security scanner for agent-generated skills"""

    def __init__(self, quarantine_dir: Optional[Path] = None):
        self.quarantine_dir = quarantine_dir

    @staticmethod
    def _sanitize_skill_name(name: str) -> str:
        """Sanitize skill name to prevent path traversal attacks."""
        sanitized = re.sub(r'[^a-zA-Z0-9_\-]', '_', name)
        if not sanitized:
            raise ValueError(f"Invalid skill name: {name!r}")
        return sanitized

    def vet_skill(
        self, skill_name: str, content: str
    ) -> tuple[VetterResult, List[SecurityFinding]]:
        """Vet a skill file for security issues

        Returns:
            (overall_result, list of findings)
        """
        findings: List[SecurityFinding] = []

        lines = content.split('\n')
        for i, line in enumerate(lines, 1):
            for category, severity, compiled_patterns in _COMPILED_PATTERNS:
                for pattern in compiled_patterns:
                    if pattern.search(line):
                        findings.append(SecurityFinding(
                            category=category,
                            severity=severity,
                            description=f"Potential {category}: {pattern.pattern}",
                            line_number=i,
                            pattern_matched=pattern.pattern,
                        ))

        # Determine overall result
        if any(f.severity == VetterResult.FAIL for f in findings):
            overall = VetterResult.FAIL
        elif any(f.severity == VetterResult.WARN for f in findings):
            overall = VetterResult.WARN
        else:
            overall = VetterResult.PASS

        logger.info(
            f"Skill '{skill_name}' vetting: {overall.value} "
            f"({len(findings)} findings)"
        )
        return overall, findings

    def quarantine_skill(
        self,
        skill_name: str,
        content: str,
        findings: List[SecurityFinding],
    ) -> Optional[Path]:
        """Move a failed skill to quarantine"""
        if not self.quarantine_dir:
            logger.warning("No quarantine directory configured")
            return None

        safe_name = self._sanitize_skill_name(skill_name)

        self.quarantine_dir.mkdir(parents=True, exist_ok=True)
        quarantine_path = self.quarantine_dir / f"{safe_name}.md"

        # Add findings header
        header = f"# QUARANTINED: {safe_name}\n\n"
        header += "## Security Findings\n\n"
        for f in findings:
            header += (
                f"- [{f.severity.value.upper()}] {f.category}: "
                f"{f.description}"
            )
            if f.line_number:
                header += f" (line {f.line_number})"
            header += "\n"
        header += "\n---\n\n## Original Content\n\n"

        quarantine_path.write_text(header + content, encoding="utf-8")
        logger.warning(f"Skill quarantined: {quarantine_path}")
        return quarantine_path

    def vet_and_activate(
        self,
        skill_name: str,
        content: str,
        skills_dir: Path,
    ) -> tuple[VetterResult, Optional[Path]]:
        """Vet a skill and activate or quarantine it

        Returns:
            (result, path where skill was saved)
        """
        safe_name = self._sanitize_skill_name(skill_name)
        result, findings = self.vet_skill(safe_name, content)

        if result == VetterResult.PASS:
            path = skills_dir / f"{safe_name}.md"
            path.write_text(content, encoding="utf-8")
            return result, path
        elif result == VetterResult.WARN:
            # Activate but mark with warning
            path = skills_dir / f"{safe_name}.md"
            warning_header = (
                f"<!-- WARNING: This skill has {len(findings)} "
                f"security warnings -->\n\n"
            )
            path.write_text(warning_header + content, encoding="utf-8")
            return result, path
        else:
            # FAIL - quarantine
            path = self.quarantine_skill(safe_name, content, findings)
            return result, path
