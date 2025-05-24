import requests
import os
import logging
from dotenv import load_dotenv
from datetime import datetime, timedelta
from ergo_helpers import get_balance, get_transactions, get_last_transaction_details
import threading

load_dotenv()

webhook_url = os.getenv("DISCORD_WEBHOOK_URL")
user_id = os.getenv("DISCORD_USER")

# List of wallets with their names and addresses
wallets = {
    "Ergo_Mining_Wallet": os.getenv("Ergo_Mining_Wallet"),
    "Ergo_Grid_Wallet": os.getenv("Ergo_Grid_Wallet"),
    "Ergo_Bot_Wallet": os.getenv("Ergo_Bot_Wallet"),
    "Ergo_Rosen_Wallet": os.getenv("Ergo_Rosen_Wallet")
}

def send_discord_message(webhook_url, message, user_id=None):
    headers = {
        'Content-Type': 'application/json'
    }

    if user_id:
        message = f"<@{user_id}> {message}"
    
    payload = {
        "content": message
    }
    
    response = requests.post(webhook_url, json=payload, headers=headers)
    
    if response.status_code == 204:
        logging.debug("Message sent successfully!")
    else:
        logging.error(f"Failed to send message. Status code: {response.status_code}")

def check_for_new_transaction(wallet_name, wallet_address):
    global last_checked_timestamps

    logging.debug(f"Starting check for new transaction in wallet: {wallet_name}")

    ergo_mining_balance = get_balance(wallet_address)
    ergo_mining_transactions = get_transactions(wallet_address)
    last_transaction_details = get_last_transaction_details(wallet_address)

    if last_transaction_details and 'timestamp' in last_transaction_details:
        timestamp = int(last_transaction_details['timestamp']) / 1000
        formatted_timestamp = datetime.fromtimestamp(timestamp).strftime('%Y-%m-%d %H:%M:%S')
        last_transaction_details['timestamp'] = formatted_timestamp

        # Check if the new transaction is after the last checked timestamp
        if wallet_name not in last_checked_timestamps or datetime.fromtimestamp(timestamp) > last_checked_timestamps[wallet_name]:
            message = (
                "\n"
                "+-------------------------------------------------+\n"
                f"|  Transaction Received   | [{wallet_name}]\n"
                "+-------------------------------------------------+\n\n"
                f"Ergo mining balance: ERG {ergo_mining_balance}\n"
                f"Total transactions: {ergo_mining_transactions}\n"
                "Last transaction details:\n"
                f"Transaction ID: [{last_transaction_details.get('transaction_id', 'N/A')}](https://ergexplorer.com/transactions/{last_transaction_details.get('transaction_id', 'N/A')})\n"
                f"Timestamp: {last_transaction_details.get('timestamp', 'N/A')}\n"
                f"Value: ERG {last_transaction_details.get('value', 'N/A')}"
            )

            send_discord_message(webhook_url, message, user_id)
            last_checked_timestamps[wallet_name] = datetime.fromtimestamp(timestamp)

    # Log the next check time
    next_check_time = datetime.now() + timedelta(seconds=300)
    logging.debug(f"Scheduled next check for wallet {wallet_name} at: {next_check_time}")

    # Schedule the next check
    threading.Timer(300, check_for_new_transaction, args=(wallet_name, wallet_address)).start()

if __name__ == "__main__":
    logging.basicConfig(filename='discord.log', level=logging.DEBUG, format='%(asctime)s - %(levelname)s - %(message)s')
    last_checked_timestamps = {}

    # Start the first check for each wallet
    for wallet_name, wallet_address in wallets.items():
        if not wallet_address:
            logging.warning(f"Skipping {wallet_name} as no address is provided.")
            continue
        check_for_new_transaction(wallet_name, wallet_address)
    
    # Keep the main thread alive
    while True:
        pass
