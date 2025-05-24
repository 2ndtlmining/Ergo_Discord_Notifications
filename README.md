# Ergo Transaction Checker

## Description

The Ergo Transaction Checker is a Python script designed to monitor specified Ergo wallets for new transactions and send notifications via Discord. It periodically checks each wallet, retrieves the balance and transaction details, and sends a message to a specified Discord channel if a new transaction is detected.

## Requirements

- Python 3.x
- `requests` library
- `python-dotenv` library
- `logging` module (built-in with Python)
- `threading` module (built-in with Python)
- `Discord Server Webhook` to send notifications to Discord.


You can install the required libraries using pip:

```sh
pip install requests python-dotenv
```
or
```sh
pip install -r requirements.txt ```



## Setup

1. Clone the Repository: Clone this repository to your local machine.
``git clone <repository-url>``
``cd ergo-transaction-checker``

2. Create a `.env` file in the root directory of the project and add the following environment variables:

``DISCORD_WEBHOOK_URL=<your-discord-webhook-url>
DISCORD_USER=<your-discord-user-id>
Ergo_Mining_Wallet=<ergo-mining-wallet-address>
Ergo_Grid_Wallet=<ergo-grid-wallet-address>
Ergo_Bot_Wallet=<ergo-bot-wallet-address>
Ergo_Rosen_Wallet=<ergo-rosen-wallet-address>
``

3. Run the script: You can run the script using the following command:

``pip install -r requirements.txt``

## Execution

1. Run the Script: Once you have installed the required libraries and set up the environment variables, you can run the script using the following command:
``python main.py``

## Note
- The script logs its activities to a file named `discord.log`
- Each wallet is checked every 5minutes 
