"""
Supabase Test Utilities

Generic database helpers for testing with Supabase.
Supports user management, email verification, and test data cleanup.
"""

import subprocess
import json
from typing import Dict, List, Optional, Any


class SupabaseTestClient:
    """
    Generic Supabase test client for database operations during testing.

    Example:
        ```python
        client = SupabaseTestClient(
            url="https://project.supabase.co",
            service_key="your-service-role-key",
            db_password="your-db-password"
        )

        # Create test user
        user_id = client.create_user("test@example.com", "password123")

        # Verify email (bypass email sending)
        client.confirm_email(user_id)

        # Cleanup after test
        client.delete_user(user_id)
        ```
    """

    def __init__(self, url: str, service_key: str, db_password: str = None, db_host: str = None):
        """
        Initialize Supabase test client.

        Args:
            url: Supabase project URL (e.g., "https://project.supabase.co")
            service_key: Service role key for admin operations
            db_password: Database password for direct SQL operations
            db_host: Database host (if different from default)
        """
        self.url = url.rstrip('/')
        self.service_key = service_key
        self.db_password = db_password

        # Extract DB host from URL if not provided
        if not db_host:
            # Convert https://abc123.supabase.co to db.abc123.supabase.co
            project_ref = url.split('//')[1].split('.')[0]
            self.db_host = f"db.{project_ref}.supabase.co"
        else:
            self.db_host = db_host

    def _run_sql(self, sql: str) -> Dict[str, Any]:
        """
        Execute SQL directly against the database.

        Args:
            sql: SQL query to execute

        Returns:
            Dictionary with 'success', 'output', 'error' keys
        """
        if not self.db_password:
            return {'success': False, 'error': 'Database password not provided'}

        try:
            result = subprocess.run(
                [
                    'psql',
                    '-h', self.db_host,
                    '-p', '5432',
                    '-U', 'postgres',
                    '-c', sql,
                    '-t',  # Tuples only
                    '-A',  # Unaligned output
                ],
                env={'PGPASSWORD': self.db_password},
                capture_output=True,
                text=True,
                timeout=10
            )

            return {
                'success': result.returncode == 0,
                'output': result.stdout.strip(),
                'error': result.stderr.strip() if result.returncode != 0 else None
            }
        except Exception as e:
            return {'success': False, 'error': str(e)}

    def create_user(self, email: str, password: str, metadata: Dict = None) -> Optional[str]:
        """
        Create a test user via Auth Admin API.

        Args:
            email: User email
            password: User password
            metadata: Optional user metadata

        Returns:
            User ID if successful, None otherwise

        Example:
            ```python
            user_id = client.create_user(
                "test@example.com",
                "SecurePass123!",
                metadata={"full_name": "Test User"}
            )
            ```
        """
        import requests

        payload = {
            'email': email,
            'password': password,
            'email_confirm': True
        }

        if metadata:
            payload['user_metadata'] = metadata

        try:
            response = requests.post(
                f"{self.url}/auth/v1/admin/users",
                headers={
                    'Authorization': f'Bearer {self.service_key}',
                    'apikey': self.service_key,
                    'Content-Type': 'application/json'
                },
                json=payload,
                timeout=10
            )

            if response.ok:
                return response.json().get('id')
            else:
                print(f"Error creating user: {response.text}")
                return None
        except Exception as e:
            print(f"Exception creating user: {e}")
            return None

    def confirm_email(self, user_id: str = None, email: str = None) -> bool:
        """
        Confirm user email (bypass email verification for testing).

        Args:
            user_id: User ID (if known)
            email: User email (alternative to user_id)

        Returns:
            True if successful, False otherwise

        Example:
            ```python
            # By user ID
            client.confirm_email(user_id="abc-123")

            # Or by email
            client.confirm_email(email="test@example.com")
            ```
        """
        if user_id:
            sql = f"UPDATE auth.users SET email_confirmed_at = NOW() WHERE id = '{user_id}';"
        elif email:
            sql = f"UPDATE auth.users SET email_confirmed_at = NOW() WHERE email = '{email}';"
        else:
            return False

        result = self._run_sql(sql)
        return result['success']

    def delete_user(self, user_id: str = None, email: str = None) -> bool:
        """
        Delete a test user and related data.

        Args:
            user_id: User ID
            email: User email (alternative to user_id)

        Returns:
            True if successful, False otherwise

        Example:
            ```python
            client.delete_user(email="test@example.com")
            ```
        """
        # Get user ID if email provided
        if email and not user_id:
            result = self._run_sql(f"SELECT id FROM auth.users WHERE email = '{email}';")
            if result['success'] and result['output']:
                user_id = result['output'].strip()
            else:
                return False

        if not user_id:
            return False

        # Delete from profiles first (foreign key)
        self._run_sql(f"DELETE FROM public.profiles WHERE id = '{user_id}';")

        # Delete from auth.users
        result = self._run_sql(f"DELETE FROM auth.users WHERE id = '{user_id}';")

        return result['success']

    def cleanup_related_records(self, user_id: str, tables: List[str] = None) -> Dict[str, bool]:
        """
        Clean up user-related records from multiple tables.

        Args:
            user_id: User ID
            tables: List of tables to clean (defaults to common tables)

        Returns:
            Dictionary mapping table names to cleanup success status

        Example:
            ```python
            results = client.cleanup_related_records(
                user_id="abc-123",
                tables=["profiles", "team_members", "coach_verification_requests"]
            )
            ```
        """
        if not tables:
            tables = [
                'pending_profiles',
                'coach_verification_requests',
                'team_members',
                'team_join_requests',
                'profiles'
            ]

        results = {}

        for table in tables:
            # Try both user_id and id columns
            sql = f"DELETE FROM public.{table} WHERE user_id = '{user_id}' OR id = '{user_id}';"
            result = self._run_sql(sql)
            results[table] = result['success']

        return results

    def create_invite_code(self, code: str, code_type: str = 'general', max_uses: int = 999) -> bool:
        """
        Create an invite code for testing.

        Args:
            code: Invite code string
            code_type: Type of code (e.g., 'general', 'team_join')
            max_uses: Maximum number of uses

        Returns:
            True if successful, False otherwise

        Example:
            ```python
            client.create_invite_code("TEST2024", code_type="general")
            ```
        """
        sql = f"""
        INSERT INTO public.invite_codes (code, code_type, is_valid, max_uses, expires_at)
        VALUES ('{code}', '{code_type}', true, {max_uses}, NOW() + INTERVAL '30 days')
        ON CONFLICT (code) DO UPDATE SET is_valid=true, max_uses={max_uses}, use_count=0;
        """

        result = self._run_sql(sql)
        return result['success']

    def find_user_by_email(self, email: str) -> Optional[str]:
        """
        Find user ID by email address.

        Args:
            email: User email

        Returns:
            User ID if found, None otherwise
        """
        sql = f"SELECT id FROM auth.users WHERE email = '{email}';"
        result = self._run_sql(sql)

        if result['success'] and result['output']:
            return result['output'].strip()
        return None

    def get_user_privileges(self, user_id: str) -> Optional[List[str]]:
        """
        Get user's privilege array.

        Args:
            user_id: User ID

        Returns:
            List of privileges if found, None otherwise
        """
        sql = f"SELECT privileges FROM public.profiles WHERE id = '{user_id}';"
        result = self._run_sql(sql)

        if result['success'] and result['output']:
            # Parse PostgreSQL array format
            privileges_str = result['output'].strip('{}')
            return [p.strip() for p in privileges_str.split(',')]
        return None


def quick_cleanup(email: str, db_password: str, project_url: str) -> bool:
    """
    Quick cleanup helper - delete user and all related data.

    Args:
        email: User email to delete
        db_password: Database password
        project_url: Supabase project URL

    Returns:
        True if successful, False otherwise

    Example:
        ```python
        from utils.supabase import quick_cleanup

        # Clean up test user
        quick_cleanup(
            "test@example.com",
            "db_password",
            "https://project.supabase.co"
        )
        ```
    """
    client = SupabaseTestClient(
        url=project_url,
        service_key="",  # Not needed for SQL operations
        db_password=db_password
    )

    user_id = client.find_user_by_email(email)
    if not user_id:
        return True  # Already deleted

    # Clean up all related tables
    client.cleanup_related_records(user_id)

    # Delete user
    return client.delete_user(user_id)
